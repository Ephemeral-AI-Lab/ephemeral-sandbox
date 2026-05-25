"""Daemon runtime API for ``api.plugin.ensure`` and ``api.plugin.status``.

``api.plugin.ensure {"plugin": "<name>"}`` imports the plugin's
``runtime/server.py`` (which decorates handlers with
:func:`sandbox.ephemeral_workspace.plugin.op_registry.register_plugin_op`) and flushes the pending
registrations into the daemon dispatcher under the public op name
``plugin.<name>.<op>``. Idempotent — re-calling for an already-loaded plugin
is a no-op.

``api.plugin.status {}`` returns the set of loaded plugins and their op names.
"""

from __future__ import annotations

import asyncio
import importlib
import inspect
import logging
import re
import sys
from collections import OrderedDict
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from sandbox.daemon.layer_stack_runtime import get_layer_stack_manager
from sandbox.ephemeral_workspace.pipeline import get_sandbox_overlay
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBindingError,
    require_workspace_binding,
)
from sandbox.ephemeral_workspace.plugin.op_context import (
    PluginOpContext,
    caller_from_audit_payload,
    plugin_intent_from_payload,
)
from sandbox.ephemeral_workspace.plugin.op_registry import (
    clear_plugin_registrations,
    flush_plugin_registrations,
    pending_plugin_registrations,
)
from sandbox.ephemeral_workspace.plugin.projection import WorkspaceProjection

__all__ = [
    "PluginEnsureError",
    "plugin_ensure",
    "plugin_status",
]


logger = logging.getLogger(__name__)


class PluginEnsureError(RuntimeError):
    """Raised when api.plugin.ensure fails to load a plugin runtime."""


@dataclass
class _LoadedPluginRuntime:
    ops: list[str]
    digest: str


_LOADED_PLUGIN_RUNTIMES: dict[str, _LoadedPluginRuntime] = {}
# Per-layer-stack-root WorkspaceProjection cache so stateful plugin runtimes
# reuse the same projection across calls.
_MAX_WORKSPACE_PROJECTIONS = 256
_MAX_AUDIT_FIELD_CHARS = 256
_PLUGIN_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_WORKSPACE_PROJECTIONS: OrderedDict[str, WorkspaceProjection] = OrderedDict()
# WR-01: per-plugin async lock so two concurrent ensure calls with
# different digests cannot interleave at await boundaries. Without this
# the digest-A → digest-B race tore the dispatcher (T2's unload popped
# entries T1 just registered).
_PLUGIN_ENSURE_LOCKS: dict[str, asyncio.Lock] = {}


async def plugin_ensure(args: dict[str, Any]) -> dict[str, Any]:
    plugin_name = str(args.get("plugin") or "").strip()
    if not plugin_name:
        raise PluginEnsureError("api.plugin.ensure requires plugin name")
    _validate_plugin_name(plugin_name)
    digest = str(args.get("digest") or "").strip()
    lock = _PLUGIN_ENSURE_LOCKS.setdefault(plugin_name, asyncio.Lock())
    async with lock:
        return await _plugin_ensure_locked(plugin_name, digest, args)


async def _plugin_ensure_locked(
    plugin_name: str,
    digest: str,
    args: dict[str, Any],
) -> dict[str, Any]:
    loaded = _LOADED_PLUGIN_RUNTIMES.get(plugin_name)
    if loaded is not None and (not digest or loaded.digest == digest):
        warm_result = await _warm_plugin_runtime(plugin_name, args)
        return {
            "success": True,
            "plugin": plugin_name,
            "digest": loaded.digest,
            "registered_ops": list(loaded.ops),
            "runtime_loaded": True,
            "already_loaded": True,
            **warm_result,
        }
    if loaded is not None:
        await _unload_plugin_runtime(plugin_name)

    runtime_module = f"plugins.catalog.{plugin_name}.runtime.server"
    runtime_loaded = False
    try:
        importlib.import_module(runtime_module)
        runtime_loaded = True
    except ModuleNotFoundError:
        # Manifest declared no runtime (or runtime layout is missing). The
        # registrations dict will be empty for stateless plugins.
        runtime_loaded = False
    except Exception as exc:  # pragma: no cover - surface the import error
        raise PluginEnsureError(
            f"plugin runtime import failed for {plugin_name!r}: {exc}"
        ) from exc

    register_op = _import_dispatcher_register_op()
    registered_ops = flush_plugin_registrations(
        plugin_name,
        register_op,
        context_factory=_plugin_op_context_factory,
        trusted_caller=True,
    )
    # Warm before writing _LOADED_PLUGIN_RUNTIMES so a failed warm doesn't
    # wedge the registry (BL-01). On warm failure roll back the dispatcher
    # entries we just registered so the next call retries cleanly.
    try:
        warm_result = (
            await _warm_plugin_runtime(plugin_name, args)
            if runtime_loaded
            else {"runtime_warmed": False}
        )
    except Exception:
        from sandbox.daemon.rpc.dispatcher import OP_TABLE

        for op in registered_ops:
            OP_TABLE.pop(op, None)
        _evict_plugin_runtime_modules(plugin_name)
        importlib.invalidate_caches()
        raise
    _LOADED_PLUGIN_RUNTIMES[plugin_name] = _LoadedPluginRuntime(
        ops=registered_ops,
        digest=digest,
    )
    if not registered_ops and not runtime_loaded:
        # Stateless plugin with no runtime — fine, idempotent.
        logger.debug(
            "plugin %s: no runtime, no ops registered", plugin_name
        )
    return {
        "success": True,
        "plugin": plugin_name,
        "digest": digest,
        "registered_ops": list(registered_ops),
        "runtime_loaded": runtime_loaded,
        "already_loaded": False,
        **warm_result,
    }


async def plugin_status(args: dict[str, Any]) -> dict[str, Any]:
    del args
    return {
        "success": True,
        "loaded_plugins": [
            {"name": name, "ops": list(loaded.ops)}
            for name, loaded in sorted(_LOADED_PLUGIN_RUNTIMES.items())
        ],
        "pending": [
            {
                "plugin": entry.plugin_name,
                "op": entry.op_name,
            }
            for entry in pending_plugin_registrations()
        ],
    }


async def _warm_plugin_runtime(
    plugin_name: str,
    args: dict[str, Any],
) -> dict[str, Any]:
    """Run an optional plugin warm hook after runtime registration."""
    module = sys.modules.get(f"plugins.catalog.{plugin_name}.runtime.server")
    warm = getattr(module, "warm_plugin_runtime", None)
    if not callable(warm):
        return {"runtime_warmed": False}

    ctx = await _plugin_op_context_factory(args, plugin_name, "__warm__")
    try:
        result = warm(args, ctx)
        if inspect.isawaitable(result):
            result = await result
    except Exception as exc:  # pragma: no cover - surfaced through daemon
        raise PluginEnsureError(
            f"plugin runtime warm failed for {plugin_name!r}: {exc}"
        ) from exc

    warm_payload = result if isinstance(result, dict) else {}
    return {
        "runtime_warmed": True,
        "warm_result": warm_payload,
    }


async def _unload_plugin_runtime(plugin_name: str) -> None:
    await _evict_plugin_sessions(plugin_name)
    from sandbox.daemon.rpc.dispatcher import OP_TABLE

    loaded = _LOADED_PLUGIN_RUNTIMES.pop(plugin_name, None)
    for op in (loaded.ops if loaded is not None else ()):
        OP_TABLE.pop(op, None)
    clear_plugin_registrations(plugin_name)
    _evict_plugin_runtime_modules(plugin_name)
    importlib.invalidate_caches()


def _evict_plugin_runtime_modules(plugin_name: str) -> None:
    prefix = f"plugins.catalog.{plugin_name}"
    for module_name in [
        name
        for name in sys.modules
        if name == prefix or name.startswith(f"{prefix}.")
    ]:
        sys.modules.pop(module_name, None)


async def _evict_plugin_sessions(plugin_name: str) -> None:
    module = sys.modules.get(
        f"plugins.catalog.{plugin_name}.runtime.session_manager"
    )
    evict_all = getattr(module, "evict_all", None)
    if not callable(evict_all):
        return
    result = evict_all()
    if inspect.isawaitable(result):
        await result


def _import_dispatcher_register_op() -> Any:
    from sandbox.daemon.rpc.dispatcher import register_op

    def _idempotent_register_op(op: str, handler: Any) -> None:
        from sandbox.daemon.rpc.dispatcher import OP_TABLE

        existing = OP_TABLE.get(op)
        if existing is handler:
            return
        register_op(op, handler)

    return _idempotent_register_op


async def _plugin_op_context_factory(
    args: dict[str, Any], _plugin_name: str, op_name: str
) -> PluginOpContext:
    """Build a PluginOpContext from the daemon-envelope args.

    Plugin handlers don't see (or need) the layer_stack_root / caller fields
    directly — they're stripped from the args mapping before reaching the
    plugin handler (the dispatcher passes the raw envelope, but the wrapper
    in registry._wrap_with_context forwards the same dict).
    """
    layer_stack_root = str(args.get("layer_stack_root", "")).strip()
    caller = caller_from_audit_payload(
        args.get("caller"),
        field_reader=_audit_field,
    )
    projection = _workspace_projection_for_root(layer_stack_root)
    overlay = await _overlay_pipeline_for_root(
        layer_stack_root,
        workspace_root=str(args.get("workspace_root", "")),
    )
    return PluginOpContext(
        layer_stack_root=layer_stack_root,
        caller=caller,
        projection=projection,
        overlay=overlay,
        intent=plugin_intent_from_payload(args.get("intent")),
        metadata={
            "op_name": op_name,
            "workspace_root": str(args.get("workspace_root", "")),
        },
    )


def _workspace_projection_for_root(layer_stack_root: str) -> WorkspaceProjection:
    key = _validated_layer_stack_root(layer_stack_root)
    projection = _WORKSPACE_PROJECTIONS.get(key)
    if projection is None:
        # Share the daemon's cached LayerStack so the plugin path
        # doesn't open a second writer flock + transaction RLock over the
        # same storage root; the previous behavior (constructing a fresh
        # manager) left the lock leaked on LRU eviction.
        manager = get_layer_stack_manager(key)
        projection = WorkspaceProjection(key, manager=manager)
        _WORKSPACE_PROJECTIONS[key] = projection
        if len(_WORKSPACE_PROJECTIONS) > _MAX_WORKSPACE_PROJECTIONS:
            _WORKSPACE_PROJECTIONS.popitem(last=False)
    else:
        _WORKSPACE_PROJECTIONS.move_to_end(key)
    return projection


async def _overlay_pipeline_for_root(
    layer_stack_root: str,
    *,
    workspace_root: str,
) -> Any:
    key = _validated_layer_stack_root(layer_stack_root)
    try:
        return await get_sandbox_overlay(
            key,
            workspace_root=str(workspace_root or "").strip() or None,
            # Plugin calls use EphemeralPipeline as the daemon-owned operation
            # facade. Starting the persistent workspace mount here would create
            # a long-lived lease whose foreign-publish watcher remounts to the
            # newest full stack and blocks auto-squash under normal writes.
            start=False,
        )
    except WorkspaceBindingError as exc:
        raise PluginEnsureError(str(exc)) from exc


def _validated_layer_stack_root(layer_stack_root: str) -> str:
    if not layer_stack_root:
        raise PluginEnsureError("plugin op context requires layer_stack_root")
    root = Path(layer_stack_root).resolve(strict=False)
    try:
        binding = require_workspace_binding(root)
    except WorkspaceBindingError as exc:
        raise PluginEnsureError(str(exc)) from exc
    if Path(binding.layer_stack_root).resolve(strict=False) != root:
        raise PluginEnsureError(
            "workspace binding layer_stack_root mismatch: "
            f"{binding.layer_stack_root} != {root}"
        )
    return root.as_posix()


def _validate_plugin_name(plugin_name: str) -> None:
    if _PLUGIN_NAME_RE.fullmatch(plugin_name) is None:
        raise PluginEnsureError(f"invalid plugin name: {plugin_name!r}")


def _audit_field(caller_dict: Mapping[str, Any], key: str) -> str:
    value = str(caller_dict.get(key, ""))
    if "\x00" in value:
        raise PluginEnsureError(f"caller field {key} contains NUL byte")
    if len(value) > _MAX_AUDIT_FIELD_CHARS:
        raise PluginEnsureError(
            f"caller field {key} exceeds {_MAX_AUDIT_FIELD_CHARS} characters"
        )
    return value
