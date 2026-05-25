"""Host-side plugin dispatch for the public ``call_plugin`` facade.

Implements the 5-step sequence from
``docs/architecture/plugins-refactor.md`` §5:

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
import json
import logging
from collections.abc import Callable, Mapping
from typing import Any

from plugins.core.discovery import discover_plugins
from plugins.core.manifest import PluginManifest
from sandbox._shared.models import Intent
from sandbox.host.daemon_client import (
    DEFAULT_LAYER_STACK_ROOT,
    call_daemon_api,
)
from sandbox.ephemeral_workspace.plugin.install import PluginInstallError, ensure_installed
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import ToolResult
from tools.sandbox._lib.session import (
    caller_from_context,
    sandbox_id_or_error,
)

__all__ = [
    "call_plugin",
    "call_plugin_write",
    "forget",
    "manifest_for",
]


logger = logging.getLogger(__name__)

_PLUGIN_MANIFESTS_BY_NAME: dict[str, PluginManifest] | None = None
_RUNTIME_DIGEST_BY_SANDBOX_PLUGIN: dict[tuple[str, str], str] = {}
_PLUGIN_CALL_LOCKS: dict[tuple[str, str], asyncio.Lock] = {}
_MAX_RESPONSE_BYTES = 8 * 1024 * 1024


def manifest_for(plugin_name: str) -> PluginManifest:
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
) -> ToolResult:
    """Call a plugin op end-to-end. See module docstring for the 5-step flow."""
    sandbox_id, error = sandbox_id_or_error(context)
    if error is not None:
        return error

    layer_stack_root = (
        str(context.get("layer_stack_root") or "").strip()
        or DEFAULT_LAYER_STACK_ROOT
    )
    try:
        manifest = manifest_for(plugin)
    except KeyError as exc:
        return _error_result("manifest", plugin, op, str(exc))

    install_fn = install_runner or ensure_installed
    dispatch_fn = daemon_dispatcher or call_daemon_api
    lock = _PLUGIN_CALL_LOCKS.setdefault((sandbox_id, plugin), asyncio.Lock())

    async with lock:
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
            return _error_result(
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
            return _error_result("install", plugin, op, _exception_message(exc))

        if _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.get((sandbox_id, plugin)) != digest:
            try:
                await dispatch_fn(
                    sandbox_id,
                    "api.plugin.ensure",
                    {
                        "plugin": plugin,
                        "digest": digest,
                        "workspace_root": str(context.get("repo_root") or "").strip(),
                    },
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
                return _error_result("ensure-runtime", plugin, op, _exception_message(exc))
            _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN[(sandbox_id, plugin)] = digest

    caller = caller_from_context(context)
    raw_intent = context.get("__intent")
    intent_value = (
        raw_intent.value
        if isinstance(raw_intent, Intent)
        else Intent.READ_ONLY.value
    )
    payload_with_meta = {
        **dict(payload),
        "caller": caller.audit_fields(),
        "workspace_root": str(context.get("repo_root") or "").strip(),
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
        logger.warning(
            "plugin dispatch failed: sandbox=%s plugin=%s op=%s err=%s",
            sandbox_id,
            plugin,
            op,
            exc,
        )
        return _error_result("dispatch", plugin, op, _exception_message(exc))

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
        return _error_result(
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


def _wrap_response(
    response: Mapping[str, Any] | Any,
    *,
    plugin: str,
    op: str,
) -> ToolResult:
    if not isinstance(response, Mapping):
        return _error_result(
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
        return _error_result("dispatch", plugin, op, str(message))
    payload_dict = {
        key: value for key, value in response.items() if key != "timings"
    }
    try:
        output = json.dumps(payload_dict, sort_keys=True)
    except TypeError as exc:
        return _error_result("decode", plugin, op, str(exc))
    if len(output.encode("utf-8")) > _MAX_RESPONSE_BYTES:
        return _error_result(
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


def _error_result(
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


def reset_host_dispatch_cache() -> None:
    """Reset module-level caches. Used by tests to isolate state."""
    global _PLUGIN_MANIFESTS_BY_NAME
    _PLUGIN_MANIFESTS_BY_NAME = None
    _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.clear()
    _PLUGIN_CALL_LOCKS.clear()


def forget(sandbox_id: str) -> None:
    """Drop host-side plugin session state for one sandbox id."""
    sandbox_id = str(sandbox_id or "").strip()
    for key in [
        key for key in _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN if key[0] == sandbox_id
    ]:
        _RUNTIME_DIGEST_BY_SANDBOX_PLUGIN.pop(key, None)
    for key in [key for key in _PLUGIN_CALL_LOCKS if key[0] == sandbox_id]:
        _PLUGIN_CALL_LOCKS.pop(key, None)
