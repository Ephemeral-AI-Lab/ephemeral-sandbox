"""Import plugin tool modules and return their ``BaseTool`` instances.

The loader is invoked from :mod:`tools._framework.factory` during builtin registration.
It walks the discovered catalog, imports each tool module with a stable
synthetic module name (``plugins.catalog.<plugin>.tools.<stem>``), and
extracts the single ``BaseTool`` instance the module is required to expose.

Failures (import errors, missing/duplicate ``BaseTool``, name mismatches)
surface loudly with the offending file path so startup misconfiguration is
diagnosable from the traceback alone.
"""

from __future__ import annotations

import functools
import importlib.util
import sys
from collections.abc import Iterable
from pathlib import Path
from typing import Any

from plugins.core.discovery import default_catalog_dir, discover_plugins
from plugins.core.manifest import PluginManifest, ToolEntry
from sandbox._shared.clock import monotonic_now
from sandbox.audit.schema import (
    PluginSection,
    build_plugin_event,
    safe_emit,
)
from tools._framework.core.base import BaseTool

__all__ = [
    "PluginLoaderError",
    "PluginToolBindingError",
    "PluginToolImportError",
    "register_plugin_tools",
]


class PluginLoaderError(RuntimeError):
    """Base class for plugin loader failures."""


class PluginToolImportError(PluginLoaderError):
    """Raised when a plugin tool module fails to import."""


class PluginToolBindingError(PluginLoaderError):
    """Raised when a plugin tool module's BaseTool surface is wrong."""


_LOAD_CACHE: dict[Path, list[BaseTool]] = {}


def register_plugin_tools(
    catalog_dir: Path | None = None,
) -> list[BaseTool]:
    """Discover and import all plugin tools.

    Idempotent: a second call with the same catalog_dir returns the same
    list of cached BaseTool instances without re-importing.
    """
    base = (catalog_dir or default_catalog_dir()).resolve()
    cached = _LOAD_CACHE.get(base)
    if cached is not None:
        return list(cached)

    tools: list[BaseTool] = []
    for manifest in discover_plugins(base):
        tools.extend(_load_manifest_tools(manifest))

    _LOAD_CACHE[base] = list(tools)
    return tools


def _load_manifest_tools(manifest: PluginManifest) -> Iterable[BaseTool]:
    for entry in manifest.tools:
        yield _load_tool_entry(manifest, entry)


def _load_tool_entry(
    manifest: PluginManifest, entry: ToolEntry
) -> BaseTool:
    module_name = _module_name(manifest, entry)
    module = _import_from_path(module_name, entry.module)
    base_tools = _collect_base_tools(module)
    if len(base_tools) == 0:
        raise PluginToolBindingError(
            f"plugin tool module exports no BaseTool: {entry.module}"
        )
    if len(base_tools) > 1:
        raise PluginToolBindingError(
            f"plugin tool module exports {len(base_tools)} BaseTools; "
            f"expected exactly one: {entry.module}"
        )
    tool = base_tools[0]
    if tool.name != entry.name:
        raise PluginToolBindingError(
            f"plugin tool module BaseTool name {tool.name!r} does not match "
            f"manifest entry name {entry.name!r}: {entry.module}"
        )
    _install_plugin_audit_shim(tool, manifest=manifest, entry=entry)
    return tool


def _install_plugin_audit_shim(
    tool: BaseTool, *, manifest: PluginManifest, entry: ToolEntry
) -> None:
    """Wrap ``tool.execute`` with generic ``plugin.*`` daemon-ring emits.

    Generic by construction (V3 Principle 2): the emitted event family
    carries ``plugin_kind`` as a value, never a key — see V3 README
    §Subsystem section keys. Manifests declare ``kind`` via the frontmatter
    (Phase 2.6 Closer D); when absent we fall back to ``"custom"``.
    """
    plugin_id = manifest.name
    plugin_kind = manifest.kind or "custom"
    tool_name = entry.name
    original_execute = tool.execute

    @functools.wraps(original_execute)
    async def _audited_execute(
        arguments: Any,
        context: Any,
    ) -> Any:
        started = monotonic_now()
        safe_emit(
            build_plugin_event(
                "plugin.tool_invoked",
                PluginSection(
                    plugin_id=plugin_id,
                    plugin_kind=plugin_kind,
                    plugin_tool_name=tool_name,
                ),
            ),
            lane="normal",
        )
        try:
            result = await original_execute(arguments, context)
        except Exception as exc:
            duration_ms = (monotonic_now() - started) * 1000.0
            safe_emit(
                build_plugin_event(
                    "plugin.error",
                    PluginSection(
                        plugin_id=plugin_id,
                        plugin_kind=plugin_kind,
                        plugin_tool_name=tool_name,
                        duration_ms=duration_ms,
                        status="error",
                        error_kind=type(exc).__name__,
                    ),
                ),
                lane="normal",
            )
            raise
        duration_ms = (monotonic_now() - started) * 1000.0
        safe_emit(
            build_plugin_event(
                "plugin.tool_completed",
                PluginSection(
                    plugin_id=plugin_id,
                    plugin_kind=plugin_kind,
                    plugin_tool_name=tool_name,
                    duration_ms=duration_ms,
                    status="ok",
                ),
            ),
            lane="normal",
        )
        return result

    tool.execute = _audited_execute  # type: ignore[method-assign]


def _module_name(manifest: PluginManifest, entry: ToolEntry) -> str:
    stem = entry.module.stem
    return f"plugins.catalog.{manifest.name}.tools.{stem}"


def _import_from_path(module_name: str, module_path: Path) -> Any:
    cached = sys.modules.get(module_name)
    if cached is not None:
        return cached
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise PluginToolImportError(
            f"could not build import spec for {module_path}"
        )
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise PluginToolImportError(
            f"failed to import plugin tool module {module_path}: {exc}"
        ) from exc
    return module


def _collect_base_tools(module: Any) -> list[BaseTool]:
    found: list[BaseTool] = []
    seen: set[int] = set()
    for value in vars(module).values():
        if isinstance(value, BaseTool) and id(value) not in seen:
            seen.add(id(value))
            found.append(value)
    return found
