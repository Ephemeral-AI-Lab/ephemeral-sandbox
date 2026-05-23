"""Generic in-sandbox daemon dispatcher.

Host-to-guest contract: the resident AF_UNIX daemon decodes one JSON object
such as ``{"op": "overlay.run", "args": {...}}`` and dispatches the decoded
envelope here. Handlers return JSON-safe values or dataclasses matching the
public sandbox API result types.
"""

from __future__ import annotations

import dataclasses
import inspect
import logging
from collections.abc import Callable, Mapping
from types import SimpleNamespace
from typing import Any
from uuid import uuid4

from sandbox._shared.clock import monotonic_now

logger = logging.getLogger("sandbox.daemon.rpc.dispatcher")

_BOOT_T0 = monotonic_now()

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

    ``boot_t0`` overrides the module-level ``_BOOT_T0`` for the
    ``runtime.boot_to_dispatch_s`` metric. The daemon passes a per-call
    timestamp captured just before reading the request line, so the metric
    measures socket-receive + parse cost rather than the daemon's wall
    uptime — which would otherwise grow monotonically and break the
    Phase 3 pass bar (``runtime.boot_to_dispatch_s ≤ 2 ms``).
    """
    dispatch_entered_at = monotonic_now()
    validation_error, op, args_raw = _validate_envelope(envelope)
    if validation_error is not None:
        return validation_error

    handler = OP_TABLE.get(op)
    if handler is None:
        return _error("unknown_op", f"unknown op: {op}", {"op": op})

    try:
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
    except Exception as exc:
        error_id = uuid4().hex
        logger.exception(
            "daemon op failed",
            extra={"op": op, "error_id": error_id},
        )
        return _error(
            "internal_error",
            str(exc),
            {"op": op, "error_id": error_id},
        )


def _validate_envelope(
    envelope: Mapping[str, Any],
) -> tuple[dict[str, Any] | None, str, dict[str, Any]]:
    op = envelope.get("op")
    if not isinstance(op, str) or not op:
        return (
            _error(
                "invalid_envelope",
                "daemon envelope requires a non-empty string op",
            ),
            "",
            {},
        )
    args_raw = envelope.get("args", {})
    if args_raw is None:
        args_raw = {}
    if not isinstance(args_raw, dict):
        return (
            _error(
                "invalid_envelope",
                "daemon envelope args must be a JSON object",
                {"op": op},
            ),
            op,
            {},
        )
    return None, op, args_raw


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
    origin = boot_t0 if boot_t0 is not None else _BOOT_T0
    timings["runtime.boot_to_dispatch_s"] = max(0.0, dispatch_entered_at - origin)
    timings["runtime.dispatch_s"] = max(
        0.0, monotonic_now() - dispatch_entered_at
    )


def _error(
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


def _load_peer_bootstraps() -> None:
    from sandbox.plugin import handler as plugin_handler
    from sandbox.daemon.handler import (
        edit,
        glob,
        grep,
        health,
        metrics,
        overlay,
        read,
        workspace,
        write,
    )
    from sandbox.daemon.service import shell_runner, shell_job_handler
    from sandbox.isolated_workspace import handlers as iws_handlers
    from sandbox.isolated_workspace import ops_handlers as iws_ops_handlers

    bootstrap: dict[str, Handler] = {
        "api.isolated_workspace.enter": iws_handlers.enter,
        "api.isolated_workspace.exit": iws_handlers.exit_,
        "api.isolated_workspace.status": iws_handlers.status,
        "api.isolated_workspace.list_open": iws_handlers.list_open,
        "api.isolated_workspace.test_reset": iws_handlers.test_reset,
        "api.isolated_workspace.shell": iws_ops_handlers.shell,
        "api.isolated_workspace.read_file": iws_ops_handlers.read_file,
        "api.isolated_workspace.write_file": iws_ops_handlers.write_file,
        "api.isolated_workspace.edit_file": iws_ops_handlers.edit_file,
        "api.isolated_workspace.grep": iws_ops_handlers.grep,
        "api.ensure_workspace_base": workspace.ensure_workspace_base,
        "api.build_workspace_base": workspace.build_workspace_base,
        "api.prepare_workspace_snapshot": workspace.prepare_workspace_snapshot,
        "api.release_workspace_snapshot": workspace.release_workspace_snapshot,
        "api.layer_stack.fence_stale_staging": workspace.fence_stale_staging,
        "api.edit_file": edit.edit_file,
        "api.v1.edit_file": edit.edit_file,
        "api.glob": glob.glob,
        "api.v1.glob": glob.glob,
        "api.grep": grep.grep,
        "api.v1.grep": grep.grep,
        "api.layer_metrics": metrics.layer_metrics,
        "api.plugin.ensure": plugin_handler.plugin_ensure,
        "api.plugin.status": plugin_handler.plugin_status,
        "api.read_file": read.read_file,
        "api.v1.read_file": read.read_file,
        "api.runtime.ready": health.runtime_ready,
        "api.shell": shell_runner.execute_shell_api,
        "api.v1.shell": shell_runner.execute_shell_api,
        "api.shell.launch": shell_job_handler.shell_launch,
        "api.v1.shell.launch": shell_job_handler.shell_launch,
        "api.shell.poll": shell_job_handler.shell_poll,
        "api.v1.shell.poll": shell_job_handler.shell_poll,
        "api.shell.cancel": shell_job_handler.shell_cancel,
        "api.v1.shell.cancel": shell_job_handler.shell_cancel,
        "api.shell.reap": shell_job_handler.shell_reap,
        "api.v1.shell.reap": shell_job_handler.shell_reap,
        "api.shell.metrics": shell_job_handler.shell_metrics,
        "api.v1.shell.metrics": shell_job_handler.shell_metrics,
        "api.workspace_binding": workspace.workspace_binding,
        "api.write_file": write.write_file,
        "api.v1.write_file": write.write_file,
        "api.overlay.flush": overlay.flush_workspace_overlay,
        "api.overlay.stop": overlay.stop_workspace_overlay,
        "overlay.run": overlay.run_snapshot_overlay,
    }
    for op, handler in bootstrap.items():
        register_op(op, handler)


_load_peer_bootstraps()


__all__ = [
    "Handler",
    "OP_TABLE",
    "dispatch_envelope_async",
    "register_op",
]
