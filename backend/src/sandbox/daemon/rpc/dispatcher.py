"""Generic in-sandbox daemon dispatcher.

Host-to-guest contract: the resident AF_UNIX daemon decodes one JSON object
such as ``{"op": "api.v1.shell", "args": {...}}`` and dispatches the decoded
envelope here. Handlers return JSON-safe values or dataclasses matching the
public sandbox API result types.
"""

from __future__ import annotations

import dataclasses
import asyncio
import inspect
import logging
import os
from collections.abc import Callable, Mapping
from types import SimpleNamespace
from typing import Any
from uuid import uuid4

from audit.jsonl import append_jsonl_event
from sandbox.shared.clock import monotonic_now
from sandbox.daemon.rpc.in_flight import get_in_flight_registry
from sandbox.daemon.workspace_tool.dispatch import (
    LifecycleInProgressError,
    _lifecycle_in_progress_envelope,
    acquire_dispatch_slot,
)
from sandbox.daemon.workspace_tool.payloads import _agent_id_from_args
from sandbox.isolated_workspace import IsolatedWorkspaceError
from sandbox.isolated_workspace._control_plane.pipeline_registry import get_active_pipeline

logger = logging.getLogger("sandbox.daemon.rpc.dispatcher")

_DISPATCHER_BOOT_MONOTONIC = monotonic_now()

Handler = Callable[[dict[str, Any]], Any]

OP_TABLE: dict[str, Handler] = {}


def register_op(op: str, handler: Handler) -> None:
    """Register a daemon operation handler.

    Peer bootstrap modules call this at import time. Re-registering the
    *same* handler under the same op is a no-op (so bootstrap can re-run
    safely from tests); registering a *different* handler under an
    already-claimed op raises so peer collisions surface loudly.
    """
    if not isinstance(op, str) or not op:
        raise ValueError("op must be a non-empty string")
    existing = OP_TABLE.get(op)
    if existing is handler:
        return
    if existing is not None:
        raise ValueError(f"runtime op already registered: {op}")
    OP_TABLE[op] = handler


async def dispatch_envelope_async(
    envelope: Mapping[str, Any],
    *,
    boot_t0: float | None = None,
) -> dict[str, Any]:
    """Dispatch an envelope from the daemon's running asyncio loop.

    ``boot_t0`` overrides the module-level ``_DISPATCHER_BOOT_MONOTONIC`` for the
    ``runtime.boot_to_dispatch_s`` metric. The daemon passes a per-call
    timestamp captured just before reading the request line, so the metric
    measures socket-receive + parse cost rather than the daemon's wall
    uptime — which would otherwise grow monotonically and break the
    Phase 3 pass bar (``runtime.boot_to_dispatch_s ≤ 2 ms``).
    """
    dispatch_entered_at = monotonic_now()
    validation_error, op, args_raw, invocation_id = _validate_envelope(envelope)
    if validation_error is not None:
        return validation_error

    registry = get_in_flight_registry()
    task = asyncio.current_task()
    if task is not None:
        registry.register(
            invocation_id,
            task,
            agent_id=_agent_id_from_args(args_raw),
            op=op,
            background=bool(args_raw.get("background", False)),
        )
    try:
        is_plugin_op = _is_plugin_op(op)
        agent_id = _agent_id_from_args(args_raw)
        if is_plugin_op and agent_id:
            try:
                async with acquire_dispatch_slot(agent_id):
                    plugin_block = _plugin_block_decision(op, agent_id)
                    if plugin_block is not None:
                        return plugin_block
                    return await _run_handler_and_finalize(
                        op,
                        args_raw,
                        dispatch_entered_at=dispatch_entered_at,
                        boot_t0=boot_t0,
                    )
            except LifecycleInProgressError as exc:
                return _lifecycle_in_progress_envelope(exc.agent_id, op=op)
        if is_plugin_op:
            # Plugin op without an agent_id — preserve the original gate
            # behavior (emit ``workspace_lifecycle.plugin_check_unbootstrapped``
            # only when no isolated pipeline is bootstrapped). Without an
            # agent_id ``iws.get_handle("")`` is always None so the
            # decision function cannot return a block, but its audit-emit
            # side effect for the unbootstrapped case is the contract we
            # have to keep.
            _plugin_block_decision(op, agent_id)
        return await _run_handler_and_finalize(
            op,
            args_raw,
            dispatch_entered_at=dispatch_entered_at,
            boot_t0=boot_t0,
        )
    except Exception as exc:
        error_id = uuid4().hex
        logger.exception(
            "daemon op failed",
            extra={"op": op, "error_id": error_id},
        )
        return _error_envelope(
            "internal_error",
            str(exc),
            {"op": op, "error_id": error_id},
        )
    finally:
        registry.deregister(invocation_id)


def _is_plugin_op(op_name: str) -> bool:
    return op_name.startswith("api.plugin.") or op_name.startswith("plugin.")


async def _run_handler_and_finalize(
    op: str,
    args_raw: dict[str, Any],
    *,
    dispatch_entered_at: float,
    boot_t0: float | None,
) -> dict[str, Any]:
    handler = OP_TABLE.get(op)
    if handler is None:
        return _error_envelope("unknown_op", f"unknown op: {op}", {"op": op})
    result = handler(dict(args_raw))
    if inspect.isawaitable(result):
        result = await result
    jsonable = _to_response_dict(result)
    _attach_runtime_boot_timings(
        jsonable,
        dispatch_entered_at=dispatch_entered_at,
        boot_t0=boot_t0,
    )
    return jsonable


def _validate_envelope(
    envelope: Mapping[str, Any],
) -> tuple[dict[str, Any] | None, str, dict[str, Any], str]:
    op = envelope.get("op")
    if not isinstance(op, str) or not op:
        return (
            _error_envelope(
                "invalid_envelope",
                "daemon envelope requires a non-empty string op",
            ),
            "",
            {},
            "",
        )
    invocation_id = str(envelope.get("invocation_id") or "").strip()
    if not invocation_id:
        invocation_id = uuid4().hex
        logger.warning("daemon envelope missing invocation_id for op=%s", op)
    args_raw = envelope.get("args", {})
    if args_raw is None:
        args_raw = {}
    if not isinstance(args_raw, dict):
        return (
            _error_envelope(
                "invalid_envelope",
                "daemon envelope args must be a JSON object",
                {"op": op},
            ),
            op,
            {},
            invocation_id,
        )
    args_raw.setdefault("invocation_id", invocation_id)
    return None, op, args_raw, invocation_id


def _attach_runtime_boot_timings(
    response: Any,
    *,
    dispatch_entered_at: float,
    boot_t0: float | None = None,
) -> None:
    if not isinstance(response, dict):
        return
    timings = response.get("timings")
    if not isinstance(timings, dict):
        timings = {}
        response["timings"] = timings
    origin = boot_t0 if boot_t0 is not None else _DISPATCHER_BOOT_MONOTONIC
    timings["runtime.boot_to_dispatch_s"] = max(0.0, dispatch_entered_at - origin)
    timings["runtime.dispatch_s"] = max(0.0, monotonic_now() - dispatch_entered_at)


def _error_envelope(
    kind: str,
    message: str,
    details: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "success": False,
        "warnings": [],
        "timings": {},
        "error": {
            "kind": kind,
            "message": message,
            "details": details or {},
        },
    }


def _to_jsonable(obj: Any) -> Any:
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        return {k: _to_jsonable(v) for k, v in dataclasses.asdict(obj).items()}
    if isinstance(obj, SimpleNamespace):
        return {str(k): _to_jsonable(v) for k, v in vars(obj).items()}
    if isinstance(obj, (list, tuple)):
        return [_to_jsonable(v) for v in obj]
    if isinstance(obj, dict):
        return {str(k): _to_jsonable(v) for k, v in obj.items()}
    return obj


def _to_response_dict(result: Any) -> dict[str, Any]:
    jsonable = _to_jsonable(result)
    if not isinstance(jsonable, dict):
        raise TypeError("runtime handler returned a non-object response")
    return jsonable


def _plugin_block_decision(op_name: str, agent_id: str) -> dict[str, Any] | None:
    """Phase 4 §D3: plugin gate — must run under ``acquire_dispatch_slot``.

    Caller holds the per-agent dispatch slot, so ``state.exit_pending``
    cannot flip mid-call (exit blocks on the entry_lock that this slot
    holds for its bookkeeping). Returns the block payload when the gate
    refuses the plugin op or ``None`` to proceed.
    """
    iws = get_active_pipeline()
    if iws is None:
        _emit_plugin_gate_audit(op_name, agent_id)
        return None
    if iws.get_handle(agent_id) is not None:
        return {
            "success": False,
            "warnings": [],
            "timings": {},
            "error": {
                "kind": "forbidden_in_isolated_workspace",
                "message": "plugin access is blocked while isolated_workspace is open",
                "details": {"op": op_name, "agent_id": agent_id},
            },
        }
    return None


def _emit_plugin_gate_audit(op_name: str, agent_id: str) -> None:
    event = {
        "type": "workspace_lifecycle.plugin_check_unbootstrapped",
        "payload": {"op": op_name, "agent_id": agent_id},
    }
    append_jsonl_event(os.environ.get("EOS_WORKSPACE_LIFECYCLE_AUDIT_PATH"), event)


def _isolated_workspace_error_payload(exc: object) -> dict[str, Any]:
    return {
        "success": False,
        "error": {
            "kind": getattr(exc, "kind", "internal_error"),
            "message": str(exc),
            "details": getattr(exc, "details", {}),
        },
    }


async def _isolated_workspace_enter(args: dict[str, Any]) -> dict[str, Any]:
    from sandbox.isolated_workspace._control_plane.pipeline_registry import (
        ensure_pipeline,
        require_isolated_workspace_arg,
    )

    try:
        pipeline = await ensure_pipeline(args)
        handle = await pipeline.enter(require_isolated_workspace_arg(args, "agent_id"))
    except IsolatedWorkspaceError as exc:
        return _isolated_workspace_error_payload(exc)
    return {
        "success": True,
        "manifest_version": handle.manifest_version,
        "manifest_root_hash": handle.manifest_root_hash,
    }


async def _isolated_workspace_exit(args: dict[str, Any]) -> dict[str, Any]:
    from sandbox.isolated_workspace._control_plane.pipeline_registry import (
        require_isolated_workspace_arg,
        require_pipeline,
    )

    try:
        return await require_pipeline().exit(require_isolated_workspace_arg(args, "agent_id"))
    except IsolatedWorkspaceError as exc:
        return _isolated_workspace_error_payload(exc)


async def _isolated_workspace_status(args: dict[str, Any]) -> dict[str, Any]:
    from sandbox.isolated_workspace._control_plane.pipeline_registry import (
        require_isolated_workspace_arg,
        require_pipeline,
    )

    try:
        pipeline = require_pipeline()
    except IsolatedWorkspaceError as exc:
        return _isolated_workspace_error_payload(exc)
    handle = pipeline.get_handle(require_isolated_workspace_arg(args, "agent_id"))
    if handle is None:
        return {"success": True, "open": False}
    return {
        "success": True,
        "open": True,
        "manifest_version": handle.manifest_version,
        "created_at": handle.created_at,
        "last_activity": handle.last_activity,
    }


async def _isolated_workspace_list_open(args: dict[str, Any]) -> dict[str, Any]:
    from sandbox.isolated_workspace._control_plane.pipeline_registry import (
        require_pipeline,
    )

    try:
        pipeline = require_pipeline()
    except IsolatedWorkspaceError:
        return {"success": True, "open_agent_ids": []}
    return {"success": True, "open_agent_ids": pipeline.list_open_agents()}


async def _isolated_workspace_test_reset(args: dict[str, Any]) -> dict[str, Any]:
    if os.environ.get("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "").strip().lower() != "true":
        return {
            "success": False,
            "error": {
                "kind": "forbidden",
                "message": (
                    "api.isolated_workspace.test_reset requires "
                    "EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true"
                ),
                "details": {},
            },
        }
    from sandbox.isolated_workspace._control_plane.pipeline_registry import (
        require_pipeline,
    )

    try:
        pipeline = require_pipeline()
    except IsolatedWorkspaceError:
        return {"success": True, "exited_agents": []}
    result = await pipeline.test_reset()
    return {"success": True, **result}


def _audit_pull_handler(args: dict[str, Any]) -> dict[str, Any]:
    from sandbox.daemon.audit_buffer import get_audit_buffer

    after_seq = int(args.get("after_seq", -1))
    limit = int(args.get("limit", 1000))
    result = get_audit_buffer().pull(after_seq=after_seq, limit=limit)
    result["success"] = True
    return result


def _audit_snapshot_handler(args: dict[str, Any]) -> dict[str, Any]:
    from sandbox.daemon.audit_buffer import get_audit_buffer

    result = get_audit_buffer().snapshot()
    result["success"] = True
    return result


def _audit_reset_floor_handler(args: dict[str, Any]) -> dict[str, Any]:
    if os.environ.get("EOS_DAEMON_AUDIT_ALLOW_FLOOR_RESET", "").strip().lower() != "true":
        return _error_envelope(
            "forbidden",
            "api.audit.reset_floor requires EOS_DAEMON_AUDIT_ALLOW_FLOOR_RESET=true",
            {"op": "api.audit.reset_floor"},
        )
    return {"success": True, "warnings": [], "timings": {}}


def _register_builtin_operations() -> None:
    from sandbox.daemon import builtin_operations
    from sandbox.daemon.audit_buffer import get_audit_buffer
    from sandbox.daemon.audit_schema import DaemonSection, build_daemon_event
    from sandbox.ephemeral_workspace.plugin import runtime_api

    builtin_ops: dict[str, Handler] = {
        "api.isolated_workspace.enter": _isolated_workspace_enter,
        "api.isolated_workspace.exit": _isolated_workspace_exit,
        "api.isolated_workspace.status": _isolated_workspace_status,
        "api.isolated_workspace.list_open": _isolated_workspace_list_open,
        "api.isolated_workspace.test_reset": _isolated_workspace_test_reset,
        **builtin_operations.WORKSPACE_TOOL_OPS,
        "api.ensure_workspace_base": builtin_operations.ensure_workspace_base,
        "api.build_workspace_base": builtin_operations.build_workspace_base,
        "api.acquire_snapshot": builtin_operations.acquire_snapshot,
        "api.commit_to_workspace": builtin_operations.commit_to_workspace,
        "api.release_lease": builtin_operations.release_lease,
        "api.layer_stack.fence_stale_staging": builtin_operations.fence_stale_staging,
        "api.layer_metrics": builtin_operations.layer_metrics,
        "api.plugin.ensure": runtime_api.plugin_ensure,
        "api.plugin.status": runtime_api.plugin_status,
        "api.runtime.ready": builtin_operations.runtime_ready,
        "api.v1.cancel": builtin_operations.cancel,
        "api.v1.heartbeat": builtin_operations.heartbeat,
        "api.v1.inflight_count": builtin_operations.inflight_count,
        "api.workspace_binding": builtin_operations.workspace_binding,
        "api.audit.pull": _audit_pull_handler,
        "api.audit.snapshot": _audit_snapshot_handler,
        "api.audit.reset_floor": _audit_reset_floor_handler,
    }
    for op, handler in builtin_ops.items():
        register_op(op, handler)

    buffer = get_audit_buffer()
    buffer.append(
        build_daemon_event(
            "daemon.started",
            DaemonSection(boot_epoch_id=buffer.boot_epoch_id, pid=os.getpid()),
        ),
        lane="critical",
    )


_register_builtin_operations()


__all__ = [
    "Handler",
    "OP_TABLE",
    "dispatch_envelope_async",
    "register_op",
]
