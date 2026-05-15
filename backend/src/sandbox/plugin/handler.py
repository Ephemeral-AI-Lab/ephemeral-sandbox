"""In-sandbox handlers for ``api.plugin.ensure`` and ``api.plugin.status``.

``api.plugin.ensure {"plugin": "<name>"}`` imports the plugin's
``runtime/server.py`` (which decorates handlers with
:func:`sandbox.plugin.runtime.register_plugin_op`) and flushes the pending
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
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from sandbox.daemon.workspace_server import get_layer_stack_manager
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBindingError,
    require_workspace_binding,
)
from sandbox._shared.models import SandboxCaller
from sandbox.plugin.op_context import PluginOpContext
from sandbox.plugin.op_registry import (
    clear_plugin_registrations,
    flush_plugin_registrations,
    pending_plugin_registrations,
)
from sandbox.plugin.projection import WorkspaceProjection

__all__ = [
    "PluginEnsureError",
    "loaded_plugins_snapshot",
    "plugin_ensure",
    "plugin_status",
]


logger = logging.getLogger(__name__)


class PluginEnsureError(RuntimeError):
    """Raised when api.plugin.ensure fails to load a plugin runtime."""


@dataclass
class _LoadedPlugin:
    ops: list[str]
    digest: str


_LOADED: dict[str, _LoadedPlugin] = {}
# Per-layer-stack-root WorkspaceProjection cache so plugin sessions reuse
# the same projection across calls.
_MAX_PROJECTIONS = 256
_MAX_AUDIT_FIELD_CHARS = 256
_PLUGIN_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_PROJECTIONS: OrderedDict[str, WorkspaceProjection] = OrderedDict()
# WR-01: per-plugin async lock so two concurrent ensure calls with
# different digests cannot interleave at await boundaries. Without this
# the digest-A → digest-B race tore the dispatcher (T2's unload popped
# entries T1 just registered).
_PLUGIN_LOCKS: dict[str, asyncio.Lock] = {}


async def plugin_ensure(args: dict[str, Any]) -> dict[str, Any]:
    plugin_name = str(args.get("plugin") or "").strip()
    if not plugin_name:
        raise PluginEnsureError("api.plugin.ensure requires plugin name")
    _validate_plugin_name(plugin_name)
    digest = str(args.get("digest") or "").strip()
    lock = _PLUGIN_LOCKS.setdefault(plugin_name, asyncio.Lock())
    async with lock:
        return await _plugin_ensure_locked(plugin_name, digest, args)


async def _plugin_ensure_locked(
    plugin_name: str,
    digest: str,
    args: dict[str, Any],
) -> dict[str, Any]:
    loaded = _LOADED.get(plugin_name)
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
    # Warm BEFORE writing _LOADED so a failed warm doesn't wedge the registry
    # (BL-01). On warm failure roll back the dispatcher
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
    _LOADED[plugin_name] = _LoadedPlugin(ops=registered_ops, digest=digest)
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
            for name, loaded in sorted(_LOADED.items())
        ],
        "pending": [
            {
                "plugin": entry.plugin_name,
                "op": entry.op_name,
            }
            for entry in pending_plugin_registrations()
        ],
    }


def loaded_plugins_snapshot() -> dict[str, list[str]]:
    """Read-only view of the in-process loaded-plugin map (for tests)."""
    return {name: list(loaded.ops) for name, loaded in _LOADED.items()}


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

    loaded = _LOADED.pop(plugin_name, None)
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
    caller_dict = args.get("caller") or {}
    if isinstance(caller_dict, dict):
        caller = SandboxCaller(
            agent_id=_audit_field(caller_dict, "agent_id"),
            run_id=_audit_field(caller_dict, "run_id"),
            agent_run_id=_audit_field(caller_dict, "agent_run_id"),
            task_id=_audit_field(caller_dict, "task_id"),
            task_center_run_id=_audit_field(caller_dict, "task_center_run_id"),
            task_center_task_id=_audit_field(caller_dict, "task_center_task_id"),
            task_center_attempt_id=_audit_field(caller_dict, "task_center_attempt_id"),
            task_center_mission_id=_audit_field(caller_dict, "task_center_mission_id"),
            task_center_request_id=_audit_field(caller_dict, "task_center_request_id"),
            tool_name=_audit_field(caller_dict, "tool_name"),
            tool_id=_audit_field(caller_dict, "tool_id"),
        )
    else:
        caller = SandboxCaller(agent_id="", run_id="", agent_run_id="", task_id="")
    projection = _projection_for_root(layer_stack_root)
    return PluginOpContext(
        layer_stack_root=layer_stack_root,
        caller=caller,
        projection=projection,
        metadata={
            "op_name": op_name,
            "workspace_root": str(args.get("workspace_root", "")),
        },
    )


def _projection_for_root(layer_stack_root: str) -> WorkspaceProjection:
    key = _validate_projection_root(layer_stack_root)
    projection = _PROJECTIONS.get(key)
    if projection is None:
        # Share the daemon's cached LayerStack so the plugin path
        # doesn't open a second writer flock + transaction RLock over the
        # same storage root; the previous behavior (constructing a fresh
        # manager) left the lock leaked on LRU eviction.
        manager = get_layer_stack_manager(key)
        projection = WorkspaceProjection(key, manager=manager)
        _PROJECTIONS[key] = projection
        if len(_PROJECTIONS) > _MAX_PROJECTIONS:
            _PROJECTIONS.popitem(last=False)
    else:
        _PROJECTIONS.move_to_end(key)
    return projection


def _validate_projection_root(layer_stack_root: str) -> str:
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


def _audit_field(caller_dict: dict[str, Any], key: str) -> str:
    value = str(caller_dict.get(key, ""))
    if "\x00" in value:
        raise PluginEnsureError(f"caller field {key} contains NUL byte")
    if len(value) > _MAX_AUDIT_FIELD_CHARS:
        raise PluginEnsureError(
            f"caller field {key} exceeds {_MAX_AUDIT_FIELD_CHARS} characters"
        )
    return value
