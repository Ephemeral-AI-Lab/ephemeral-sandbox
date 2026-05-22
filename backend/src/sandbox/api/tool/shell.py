"""Internal implementation for the public sandbox shell verb.

The synchronous (foreground) path issues one ``api.v1.shell`` envelope and
projects the response. The ``background=True`` path drives the daemon-native
job control surface (launch / poll / cancel / reap) so a long-running shell
can be cancelled safely without leaking the layer-stack lease.
"""

from __future__ import annotations

import asyncio
import logging

from audit.base import AuditEvent, AuditNode, AuditSink
from sandbox.api.tool.core.audit import audited_operation
from sandbox.api.tool.core.conflicts import is_shell_conflict
from sandbox.api.tool.core.daemon_response import (
    error_message,
    timings_from_daemon_response,
)
from sandbox.api.tool.core.results import (
    shell_conflict_result,
    shell_error_result,
    shell_result_from_daemon_response,
)
from sandbox.api.protocol import SandboxTransport
from sandbox.api.timeouts import shell_dispatch_timeout
from sandbox.api.transport import (
    DAEMON_OP_SHELL,
    DAEMON_OP_SHELL_CANCEL,
    DAEMON_OP_SHELL_LAUNCH,
    DAEMON_OP_SHELL_REAP,
    DaemonSandboxTransport,
)
from sandbox.audit import events as sandbox_audit_events
from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import ShellRequest, ShellResult

logger = logging.getLogger(__name__)

# Daemon side of cancel + reap should complete in well under 30 s (SIGTERM +
# 2 s grace + cleanup). Cap the dispatch budget so a hung daemon does not
# trap the engine's asyncio task indefinitely.
_BACKGROUND_CANCEL_TIMEOUT_S = 15
_BACKGROUND_REAP_AFTER_CANCEL_TIMEOUT_S = 30


async def shell(
    sandbox_id: str,
    request: ShellRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> ShellResult:
    """Run a shell command through sandbox-local overlay and OCC."""
    total_start = monotonic_now()
    cwd = (request.cwd or "").strip() or "."
    selected_transport = transport or DaemonSandboxTransport()

    async def _call() -> ShellResult:
        if request.stdin is not None:
            return shell_error_result(
                reason="stdin_not_supported",
                message="snapshot overlay shell does not accept stdin",
                timings={"api.shell.total_s": monotonic_now() - total_start},
            )
        if request.background:
            return await _shell_background_dispatch(
                sandbox_id=sandbox_id,
                request=request,
                cwd=cwd,
                transport=selected_transport,
                started_at=total_start,
                audit_sink=audit_sink,
            )
        raw = await selected_transport.call(
            sandbox_id,
            DAEMON_OP_SHELL,
            {
                "command": request.command,
                "cwd": cwd,
                "timeout_seconds": request.timeout,
                "actor_id": request.caller.agent_id,
                "caller": request.caller.audit_fields(),
                "description": request.default_description("shell"),
            },
            timeout=shell_dispatch_timeout(request.timeout),
        )
        timings = timings_from_daemon_response(raw.get("timings"))
        timings["api.shell.dispatch_total_s"] = monotonic_now() - total_start
        return shell_result_from_daemon_response(raw, timings=timings)

    def _conflict_from_error(exc: BaseException) -> ShellResult | None:
        if not is_shell_conflict(exc):
            return None
        return shell_conflict_result(
            error_message(exc),
            timings={"api.shell.dispatch_total_s": monotonic_now() - total_start},
        )

    return await audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="shell",
        caller=request.caller,
        payload={"cwd": cwd, "background": request.background},
        call=_call,
        conflict_from_error=_conflict_from_error,
    )


async def _shell_background_dispatch(
    *,
    sandbox_id: str,
    request: ShellRequest,
    cwd: str,
    transport: SandboxTransport,
    started_at: float,
    audit_sink: AuditSink | None,
) -> ShellResult:
    """Drive shell.launch -> shell.reap; handle CancelledError mid-reap."""
    launch_args = {
        "command": request.command,
        "cwd": cwd,
        "timeout_seconds": request.timeout,
        "actor_id": request.caller.agent_id,
        "caller": request.caller.audit_fields(),
        "description": request.default_description("shell.launch"),
    }
    launch_response = await transport.call(
        sandbox_id,
        DAEMON_OP_SHELL_LAUNCH,
        launch_args,
        # Launch should be near-instant: it just acquires a lease and submits
        # the strategy to the daemon's ShellExecutor. The ``+5 s`` cushion
        # covers cold-start cases (lease contention, materialize, etc.).
        timeout=20,
    )
    if not _response_ok(launch_response):
        return shell_error_result(
            reason="shell_launch_failed",
            message=_response_error_message(launch_response),
            timings={"api.shell.dispatch_total_s": monotonic_now() - started_at},
        )
    job_id = str(launch_response.get("job_id") or "")
    if not job_id:
        return shell_error_result(
            reason="shell_launch_no_job_id",
            message="daemon shell.launch did not return a job_id",
            timings={"api.shell.dispatch_total_s": monotonic_now() - started_at},
        )
    lease_id = str(launch_response.get("lease_id") or "")
    _emit_shell_audit(
        audit_sink,
        sandbox_audit_events.SHELL_LAUNCHED,
        sandbox_id=sandbox_id,
        caller=request.caller,
        payload={
            "job_id": job_id,
            "lease_id": lease_id,
            "request_id": request.caller.agent_id,
        },
    )

    reap_timeout = max(60, int(request.timeout) if request.timeout else 600)
    try:
        reap_response = await transport.call(
            sandbox_id,
            DAEMON_OP_SHELL_REAP,
            {"job_id": job_id, "timeout_seconds": float(reap_timeout)},
            # Add a dispatch grace on top of the reap budget; the daemon
            # signals SIGKILL on its end if the inner wait exceeds the budget.
            timeout=shell_dispatch_timeout(reap_timeout),
        )
    except asyncio.CancelledError:
        await _send_cancel_then_reap(
            transport=transport,
            sandbox_id=sandbox_id,
            job_id=job_id,
            audit_sink=audit_sink,
            caller_sandbox_id=sandbox_id,
            caller=request.caller,
        )
        raise
    timings = timings_from_daemon_response(reap_response.get("timings"))
    timings["api.shell.dispatch_total_s"] = monotonic_now() - started_at
    _emit_shell_audit(
        audit_sink,
        sandbox_audit_events.SHELL_REAPED,
        sandbox_id=sandbox_id,
        caller=request.caller,
        payload={
            "job_id": job_id,
            "status": str(reap_response.get("status") or ""),
            "changed_paths_count": len(reap_response.get("changed_paths") or ()),
        },
    )
    return _shell_result_from_reap(reap_response, timings=timings)


async def _send_cancel_then_reap(
    *,
    transport: SandboxTransport,
    sandbox_id: str,
    job_id: str,
    audit_sink: AuditSink | None,
    caller_sandbox_id: str,
    caller: object,
) -> None:
    """Best-effort cancel + reap on CancelledError. Errors are swallowed."""
    _emit_shell_audit(
        audit_sink,
        sandbox_audit_events.SHELL_CANCELLED,
        sandbox_id=caller_sandbox_id,
        caller=caller,
        payload={"job_id": job_id, "reason": "engine_cancel"},
    )
    try:
        await transport.call(
            sandbox_id,
            DAEMON_OP_SHELL_CANCEL,
            {"job_id": job_id, "reason": "engine_cancel"},
            timeout=_BACKGROUND_CANCEL_TIMEOUT_S,
        )
    except Exception:
        logger.debug("shell.cancel best-effort dispatch failed", exc_info=True)
        _emit_shell_audit(
            audit_sink,
            sandbox_audit_events.SHELL_REAPED,
            sandbox_id=caller_sandbox_id,
            caller=caller,
            payload={
                "job_id": job_id,
                "status": "cancel_reap_failed",
                "changed_paths_count": 0,
            },
        )
        return
    try:
        reap_response = await transport.call(
            sandbox_id,
            DAEMON_OP_SHELL_REAP,
            {"job_id": job_id, "timeout_seconds": 10.0},
            timeout=_BACKGROUND_REAP_AFTER_CANCEL_TIMEOUT_S,
        )
    except Exception:
        # Daemon TTL reaper will catch any residual lease; nothing we can do
        # synchronously without blocking the engine cancel path.
        logger.debug("shell.reap after cancel best-effort dispatch failed", exc_info=True)
        _emit_shell_audit(
            audit_sink,
            sandbox_audit_events.SHELL_REAPED,
            sandbox_id=caller_sandbox_id,
            caller=caller,
            payload={
                "job_id": job_id,
                "status": "cancel_reap_failed",
                "changed_paths_count": 0,
            },
        )
        return
    _emit_shell_audit(
        audit_sink,
        sandbox_audit_events.SHELL_REAPED,
        sandbox_id=caller_sandbox_id,
        caller=caller,
        payload={
            "job_id": job_id,
            "status": str(reap_response.get("status") or "cancelled"),
            "changed_paths_count": len(reap_response.get("changed_paths") or ()),
        },
    )


def _emit_shell_audit(
    audit_sink: AuditSink | None,
    event_type: str,
    *,
    sandbox_id: str,
    caller: object,
    payload: dict[str, object],
) -> None:
    """Publish a SHELL_* lifecycle event to the engine's audit bus.

    Mirrors the host-side derivation pattern used by
    ``sandbox_events_from_tool_completion``: produce the typed AuditEvent at
    the call boundary, let ``LegacySandboxAuditSink`` translate it into the
    legacy ``EventType.SANDBOX_SHELL_*`` for ``sandbox_events.jsonl``.
    """
    if audit_sink is None:
        return
    agent_id = getattr(caller, "agent_id", None)
    try:
        audit_sink.publish(
            AuditEvent(
                source="sandbox",
                type=event_type,
                node=AuditNode(sandbox_id=sandbox_id, agent_run_id=agent_id),
                payload=dict(payload),
            )
        )
    except Exception:
        logger.debug("shell audit publish failed event=%s", event_type, exc_info=True)


def _response_ok(payload: dict[str, object]) -> bool:
    if not isinstance(payload, dict):
        return False
    if not payload.get("success", False):
        return False
    return "error" not in payload


def _response_error_message(payload: dict[str, object]) -> str:
    err = payload.get("error") if isinstance(payload, dict) else None
    if isinstance(err, dict):
        msg = err.get("message")
        if isinstance(msg, str) and msg:
            return msg
    return "shell launch failed"


def _shell_result_from_reap(
    payload: dict[str, object],
    *,
    timings: dict[str, float],
) -> ShellResult:
    """Project a ``shell.reap`` response onto :class:`ShellResult`.

    Reap payloads do not carry the conflict/status fields produced by the
    foreground projection in :func:`shell_result_from_daemon_response`; we
    synthesize them from the status string the daemon already computed.
    """
    raw_status = str(payload.get("status") or "")
    derived_status = "ok" if raw_status == "finished" else (
        "cancelled" if raw_status == "cancelled" else "error"
    )
    return shell_result_from_daemon_response(
        {
            "success": raw_status == "finished",
            "exit_code": payload.get("exit_code", -1),
            "stdout": payload.get("stdout", ""),
            "stderr": payload.get("stderr", ""),
            "changed_paths": payload.get("changed_paths", []),
            "status": derived_status,
            "conflict": None,
            "conflict_reason": payload.get("error"),
            "warnings": [],
        },
        timings=timings,
    )


__all__ = ["shell"]
