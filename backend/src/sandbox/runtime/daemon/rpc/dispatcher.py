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

from sandbox.timing import monotonic_now

logger = logging.getLogger("sandbox.runtime.daemon.rpc.dispatcher")

_BOOT_T0 = monotonic_now()

Handler = Callable[[dict[str, Any]], Any]

OP_TABLE: dict[str, Handler] = {}


def register_op(op: str, handler: Handler) -> None:
    """Register a daemon operation handler.

    Peer bootstrap modules call this at import time. Dispatch remains a table
    lookup; peer-specific branching belongs in peer handlers or pipelines.
    """
    if not isinstance(op, str) or not op:
        raise ValueError("op must be a non-empty string")
    if op in OP_TABLE:
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
    from sandbox.runtime.daemon.handler import health, metrics, workspace
    from sandbox.runtime.daemon.handler import overlay as overlay_run
    from sandbox.runtime.daemon.handler.tools import edit, read, write
    from sandbox.runtime.daemon.service import shell_runner

    for op, handler in {
        "api.ensure_workspace_base": workspace.ensure_workspace_base,
        "api.build_workspace_base": workspace.build_workspace_base,
        "api.prepare_workspace_snapshot": (
            workspace.prepare_workspace_snapshot
        ),
        "api.release_workspace_snapshot": (
            workspace.release_workspace_snapshot
        ),
        "api.layer_stack.fence_stale_staging": (
            workspace.fence_stale_staging
        ),
        "api.edit_file": edit.edit_file,
        "api.v1.edit_file": edit.edit_file,
        "api.layer_metrics": metrics.layer_metrics,
        "api.plugin.ensure": plugin_handler.plugin_ensure,
        "api.plugin.status": plugin_handler.plugin_status,
        "api.read_file": read.read_file,
        "api.v1.read_file": read.read_file,
        "api.runtime.ready": health.runtime_ready,
        "api.shell": shell_runner.execute_shell_api,
        "api.v1.shell": shell_runner.execute_shell_api,
        "api.workspace_binding": workspace.workspace_binding,
        "api.write_file": write.write_file,
        "api.v1.write_file": write.write_file,
        "overlay.run": overlay_run.handle,
    }.items():
        existing = OP_TABLE.get(op)
        if existing is handler:
            continue
        if existing is not None:
            raise ValueError(f"runtime op already registered: {op}")
        register_op(op, handler)


_load_peer_bootstraps()


__all__ = [
    "Handler",
    "OP_TABLE",
    "dispatch_envelope_async",
    "register_op",
]
