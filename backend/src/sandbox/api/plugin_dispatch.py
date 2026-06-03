"""Host-side plugin dispatch for the public ``call_plugin`` facade.

Implements the host-to-daemon plugin dispatch sequence:

  1. Resolve sandbox_id + layer_stack_root + caller from the tool context.
  2. Ensure the plugin bundle is installed inside the sandbox
     (:func:`sandbox.ephemeral_workspace.plugin.install.ensure_installed`).
  3. Tell the daemon to load the plugin runtime via ``api.plugin.ensure``.
  4. Dispatch ``call_daemon_api(sandbox_id, "plugin.<name>.<op>", payload)``.
  5. Wrap the response in a :class:`ToolResult`.

Errors at any step surface as ``ToolResult(is_error=True, ...)`` with a
message that names the failing step so callers can disambiguate.
"""

from __future__ import annotations

import asyncio
import hashlib
import importlib
import json
import logging
import sys
from collections.abc import Callable, Mapping
from typing import Any

from plugins.core.discovery import discover_plugins
from plugins.core.manifest import PluginManifest
from sandbox.host.paths import BUNDLE_REMOTE_DIR
from sandbox._shared.models import Intent
from plugins.runtime_bridge.op_registry import (
    clear_plugin_registrations,
    pending_plugin_registrations,
)
from sandbox.host.daemon_client import (
    DEFAULT_LAYER_STACK_ROOT,
    call_daemon_api,
)
from sandbox.api.plugin_install import PluginInstallError, ensure_installed
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import ToolResult
from tools.sandbox._lib.tool_context import (
    sandbox_caller_from_tool_context,
    sandbox_id_or_missing_error_result,
)

__all__ = [
    "call_plugin",
    "call_plugin_write",
    "forget_plugin_dispatch_state",
]


logger = logging.getLogger(__name__)

_PLUGIN_MANIFESTS_BY_NAME: dict[str, PluginManifest] | None = None
_PluginRuntimeCacheKey = tuple[str, str, str, str]
_RUNTIME_DIGEST_BY_SANDBOX_PLUGIN: dict[_PluginRuntimeCacheKey, str] = {}
_PLUGIN_SETUP_LOCKS: dict[_PluginRuntimeCacheKey, asyncio.Lock] = {}
_MAX_RESPONSE_BYTES = 8 * 1024 * 1024


def _manifest_for(plugin_name: str) -> PluginManifest:
    """Look up a plugin manifest by name from the host catalog."""
    global _PLUGIN_MANIFESTS_BY_NAME
    if _PLUGIN_MANIFESTS_BY_NAME is None:
        _PLUGIN_MANIFESTS_BY_NAME = {m.name: m for m in discover_plugins()}
    manifest = _PLUGIN_MANIFESTS_BY_NAME.get(plugin_name)
    if manifest is None:
        raise KeyError(
            f"plugin {plugin_name!r} not found in catalog "
            f"(available: {sorted(_PLUGIN_MANIFESTS_BY_NAME)})"
        )
    return manifest


async def call_plugin(
    context: ToolExecutionContextService,
    *,
    plugin: str,
    op: str,
    payload: Mapping[str, Any],
    timeout: int = 60,
    daemon_dispatcher: Callable[..., Any] | None = None,
    install_runner: Callable[..., Any] | None = None,
    _retry_unknown_op: bool = True,
) -> ToolResult:
    """Call a plugin op end-to-end. See module docstring for the 5-step flow."""
    sandbox_id, error = sandbox_id_or_missing_error_result(context)
    if error is not None:
        return error

    layer_stack_root = (
        str(context.get("layer_stack_root") or "").strip()
        or DEFAULT_LAYER_STACK_ROOT
    )
    workspace_root = str(context.get("repo_root") or "").strip()
    runtime_cache_key = _runtime_cache_key(
        sandbox_id,
        plugin,
        layer_stack_root=layer_stack_root,
        workspace_root=workspace_root,
    )
    try:
        manifest = _manifest_for(plugin)
    except KeyError as exc:
        return _plugin_error_result("manifest", plugin, op, str(exc))

    install_fn = install_runner or ensure_installed
    dispatch_fn = daemon_dispatcher or call_daemon_api

    try:
        digest = await install_fn(sandbox_id, manifest)
    except PluginInstallError as exc:
        logger.warning(
            "plugin install failed: sandbox=%s plugin=%s op=%s err=%s",
            sandbox_id,
            plugin,
            op,
            exc,
        )
        return _plugin_error_result(
            "install",
            plugin,
            op,
            _exception_message(exc),
            details=_plugin_install_error_details(exc, plugin=plugin),
        )
    except Exception as exc:
        logger.warning(
            "plugin install failed: sandbox=%s plugin=%s op=%s err=%s",
            sandbox_id,
            plugin,
            op,
            exc,
        )
        return _plugin_error_result("install", plugin, op, _exception_message(exc))

    if _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.get(runtime_cache_key) != digest:
        setup_lock = _PLUGIN_SETUP_LOCKS.setdefault(
            runtime_cache_key, asyncio.Lock()
        )
        async with setup_lock:
            if _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.get(runtime_cache_key) == digest:
                pass
            else:
                try:
                    await dispatch_fn(
                        sandbox_id,
                        "api.plugin.ensure",
                        _ensure_payload(
                            manifest,
                            digest=digest,
                            workspace_root=workspace_root,
                        ),
                        timeout=timeout,
                        layer_stack_root=layer_stack_root,
                    )
                except Exception as exc:
                    logger.warning(
                        "plugin ensure failed: sandbox=%s plugin=%s op=%s err=%s",
                        sandbox_id,
                        plugin,
                        op,
                        exc,
                    )
                    return _plugin_error_result(
                        "ensure-runtime",
                        plugin,
                        op,
                        _exception_message(exc),
                    )
                _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN[runtime_cache_key] = digest

    caller = sandbox_caller_from_tool_context(context)
    raw_intent = context.get("__intent")
    intent_value = (
        raw_intent.value
        if isinstance(raw_intent, Intent)
        else Intent.READ_ONLY.value
    )
    payload_with_meta = {
        **dict(payload),
        "caller": caller.audit_fields(),
        "workspace_root": workspace_root,
        "intent": intent_value,
    }
    try:
        response = await dispatch_fn(
            sandbox_id,
            f"plugin.{plugin}.{op}",
            payload_with_meta,
            timeout=timeout,
            layer_stack_root=layer_stack_root,
        )
    except Exception as exc:
        if _retry_unknown_op and _is_unknown_plugin_op_error(exc):
            _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.pop(runtime_cache_key, None)
            logger.info(
                "plugin runtime cache stale; re-ensuring runtime: "
                "sandbox=%s plugin=%s op=%s",
                sandbox_id,
                plugin,
                op,
            )
            return await call_plugin(
                context,
                plugin=plugin,
                op=op,
                payload=payload,
                timeout=timeout,
                daemon_dispatcher=daemon_dispatcher,
                install_runner=install_runner,
                _retry_unknown_op=False,
            )
        logger.warning(
            "plugin dispatch failed: sandbox=%s plugin=%s op=%s err=%s",
            sandbox_id,
            plugin,
            op,
            exc,
        )
        return _plugin_error_result("dispatch", plugin, op, _exception_message(exc))

    return _wrap_response(response, plugin=plugin, op=op)


async def call_plugin_write(
    context: ToolExecutionContextService,
    *,
    plugin: str,
    op: str,
    payload: Mapping[str, Any],
    timeout: int = 60,
    daemon_dispatcher: Callable[..., Any] | None = None,
    install_runner: Callable[..., Any] | None = None,
) -> ToolResult:
    """Dispatch a mutating plugin op only from a WRITE_ALLOWED tool."""
    if context.get("__intent") is not Intent.WRITE_ALLOWED:
        return _plugin_error_result(
            "intent",
            plugin,
            op,
            "call_plugin_write requires @tool(intent=Intent.WRITE_ALLOWED)",
        )
    return await call_plugin(
        context,
        plugin=plugin,
        op=op,
        payload=payload,
        timeout=timeout,
        daemon_dispatcher=daemon_dispatcher,
        install_runner=install_runner,
    )


def _ensure_payload(
    manifest: PluginManifest,
    *,
    digest: str,
    workspace_root: str,
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "plugin": manifest.name,
        "digest": digest,
        "workspace_root": workspace_root,
    }
    daemon_manifest = _daemon_manifest_for(manifest, digest=digest)
    if daemon_manifest is not None:
        payload["manifest"] = daemon_manifest
        payload["start_services"] = True
    return payload


def _daemon_manifest_for(
    manifest: PluginManifest,
    *,
    digest: str,
) -> dict[str, Any] | None:
    if manifest.runtime is None:
        return None

    registrations = _runtime_registrations(manifest)
    if not registrations:
        return None

    service_ops = [
        entry
        for entry in registrations
        if entry.intent is Intent.READ_ONLY
        or (entry.intent is Intent.WRITE_ALLOWED and not entry.auto_workspace_overlay)
    ]
    service_id = "runtime"
    operations: list[dict[str, Any]] = []
    for entry in registrations:
        operation: dict[str, Any] = {
            "op_name": entry.op_name,
            "intent": entry.intent.value,
            "auto_workspace_overlay": bool(entry.auto_workspace_overlay),
        }
        if entry in service_ops:
            operation["service_id"] = service_id
        operations.append(operation)

    services: list[dict[str, Any]] = []
    if service_ops:
        services.append(
            {
                "service_id": service_id,
                "service_profile_digest": _service_profile_digest(manifest, digest),
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace_and_notify",
                "command": _ppc_service_command(),
                "ppc_protocol_version": 1,
            }
        )

    return {
        "plugin_id": manifest.name,
        "plugin_version": "0.1.0",
        "plugin_digest": digest,
        "services": services,
        "operations": operations,
    }


def _runtime_registrations(manifest: PluginManifest) -> tuple[Any, ...]:
    module_name = _runtime_module_name(manifest)
    if module_name in sys.modules:
        clear_plugin_registrations(manifest.name)
        module = sys.modules[module_name]
        module = importlib.reload(module)
    else:
        clear_plugin_registrations(manifest.name)
        module = importlib.import_module(module_name)
    del module
    return pending_plugin_registrations(manifest.name)


def _runtime_module_name(manifest: PluginManifest) -> str:
    runtime = manifest.runtime
    if runtime is None:
        return ""
    rel = runtime.relative_to(manifest.source_dir).with_suffix("")
    return ".".join(("plugins", "catalog", manifest.name, *rel.parts))


def _service_profile_digest(manifest: PluginManifest, digest: str) -> str:
    runtime_rel = ""
    if manifest.runtime is not None:
        runtime_rel = manifest.runtime.relative_to(manifest.source_dir).as_posix()
    source = f"{manifest.name}\0{digest}\0{runtime_rel}\0ppc-service-v1"
    return hashlib.sha256(source.encode("utf-8")).hexdigest()


def _ppc_service_command() -> list[str]:
    launcher = (
        "import sys; "
        f"sys.path.insert(0, {BUNDLE_REMOTE_DIR!r}); "
        "from plugins.runtime_bridge.ppc_service import main; "
        "raise SystemExit(main())"
    )
    return ["python3", "-c", launcher]


def _wrap_response(
    response: Mapping[str, Any] | Any,
    *,
    plugin: str,
    op: str,
) -> ToolResult:
    if not isinstance(response, Mapping):
        return _plugin_error_result(
            "decode",
            plugin,
            op,
            f"plugin response was not a mapping: {type(response).__name__}",
        )
    if response.get("error"):
        err = response.get("error") or {}
        message = (
            err.get("message")
            if isinstance(err, Mapping)
            else str(err)
        )
        return _plugin_error_result("dispatch", plugin, op, str(message))
    payload_dict = {
        key: value for key, value in response.items() if key != "timings"
    }
    try:
        output = json.dumps(payload_dict, sort_keys=True)
    except TypeError as exc:
        return _plugin_error_result("decode", plugin, op, str(exc))
    if len(output.encode("utf-8")) > _MAX_RESPONSE_BYTES:
        return _plugin_error_result(
            "decode",
            plugin,
            op,
            f"plugin response exceeds {_MAX_RESPONSE_BYTES} byte limit",
        )
    metadata: dict[str, Any] = {"plugin": plugin, "op": op}
    timings = response.get("timings")
    if isinstance(timings, Mapping):
        metadata["timings"] = dict(timings)
    return ToolResult(output=output, is_error=False, metadata=metadata)


def _plugin_error_result(
    step: str,
    plugin: str,
    op: str,
    message: str,
    *,
    details: Mapping[str, Any] | None = None,
) -> ToolResult:
    metadata: dict[str, Any] = {"plugin": plugin, "op": op, "step": step}
    if details:
        metadata["details"] = dict(details)
        error_kind = details.get("kind") or details.get("error_kind")
        if error_kind:
            metadata["error_kind"] = str(error_kind)
    return ToolResult(
        output=f"plugin {plugin}.{op} {step} failed: {message}",
        is_error=True,
        metadata=metadata,
    )


def _exception_message(exc: Exception) -> str:
    message = str(exc)
    if message:
        return message
    return exc.__class__.__name__


def _is_unknown_plugin_op_error(exc: Exception) -> bool:
    kind = str(getattr(exc, "kind", "") or "")
    message = _exception_message(exc)
    return kind == "unknown_op" and "plugin." in message


def _runtime_cache_key(
    sandbox_id: str,
    plugin: str,
    *,
    layer_stack_root: str,
    workspace_root: str,
) -> _PluginRuntimeCacheKey:
    return (sandbox_id, plugin, layer_stack_root, workspace_root)


def _plugin_install_error_details(
    exc: PluginInstallError,
    *,
    plugin: str,
) -> dict[str, str]:
    return {
        "kind": exc.kind,
        "plugin": exc.plugin_name or plugin,
        "setup_step": exc.setup_step,
        "command": exc.command,
        "stderr_excerpt": exc.stderr_excerpt,
    }


def reset_host_dispatch_cache_for_tests() -> None:
    """Reset module-level caches. Used by tests to isolate state."""
    global _PLUGIN_MANIFESTS_BY_NAME
    _PLUGIN_MANIFESTS_BY_NAME = None
    _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.clear()
    _PLUGIN_SETUP_LOCKS.clear()


def forget_plugin_dispatch_state(sandbox_id: str) -> None:
    """Drop host-side runtime-digest cache + setup singleflight locks for one sandbox."""
    sandbox_id = str(sandbox_id or "").strip()
    for key in [
        key for key in _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN if key[0] == sandbox_id
    ]:
        _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.pop(key, None)
    for key in [key for key in _PLUGIN_SETUP_LOCKS if key[0] == sandbox_id]:
        _PLUGIN_SETUP_LOCKS.pop(key, None)
