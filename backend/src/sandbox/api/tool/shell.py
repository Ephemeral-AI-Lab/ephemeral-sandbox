"""Public sandbox shell verb."""

from __future__ import annotations

import time
from collections.abc import Mapping

from audit.base import AuditSink
from sandbox.audit.operation import (
    publish_operation_failed,
    publish_operation_result,
    publish_operation_started,
)
from sandbox.api.tool._payload import (
    caller_envelope,
    conflict_from_payload,
    int_from_payload,
    paths_from_payload,
    timings_from_payload,
)
from sandbox.models import ConflictInfo, ShellRequest, ShellResult
from sandbox.host.daemon_client import call_daemon_api


async def shell(
    sandbox_id: str,
    request: ShellRequest,
    *,
    audit_sink: AuditSink | None = None,
) -> ShellResult:
    """Run a shell command through sandbox-local overlay and OCC."""
    total_start = time.perf_counter()
    publish_operation_started(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="shell",
        caller=request.caller,
        payload={"cwd": _overlay_cwd(request.cwd)},
    )
    if request.stdin is not None:
        result = _error_result(
            reason="stdin_not_supported",
            message="snapshot overlay shell does not accept stdin",
            timings={"api.shell.total_s": time.perf_counter() - total_start},
        )
        publish_operation_result(
            audit_sink,
            sandbox_id=sandbox_id,
            operation="shell",
            caller=request.caller,
            result=result,
        )
        return result

    try:
        raw = await call_daemon_api(
            sandbox_id,
            "api.shell",
            {
                "command": request.command,
                "cwd": _overlay_cwd(request.cwd),
                "timeout_seconds": request.timeout,
                "actor_id": request.caller.agent_id,
                "caller": caller_envelope(request.caller),
                "description": request.description or "shell",
            },
            timeout=(request.timeout or 60) + 30,
        )
        timings = timings_from_payload(raw.get("timings"))
        timings["api.shell.dispatch_total_s"] = time.perf_counter() - total_start
        result = _result_from_payload(raw, timings=timings)
    except Exception as exc:
        publish_operation_failed(
            audit_sink,
            sandbox_id=sandbox_id,
            operation="shell",
            caller=request.caller,
            error=exc,
        )
        raise
    publish_operation_result(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="shell",
        caller=request.caller,
        result=result,
    )
    return result


def _result_from_payload(
    raw: Mapping[str, object],
    *,
    timings: dict[str, float],
) -> ShellResult:
    conflict = conflict_from_payload(raw.get("conflict"))
    return ShellResult(
        success=bool(raw.get("success", False)),
        exit_code=int_from_payload(raw.get("exit_code"), default=1),
        stdout=str(raw.get("stdout", "")),
        stderr=str(raw.get("stderr", "")),
        changed_paths=paths_from_payload(raw.get("changed_paths")),
        status=str(raw.get("status", "")),
        conflict=conflict,
        conflict_reason=(
            str(raw.get("conflict_reason"))
            if raw.get("conflict_reason") is not None
            else None
        ),
        warnings=paths_from_payload(raw.get("warnings")),
        timings=timings,
    )


def _error_result(
    *,
    reason: str,
    message: str,
    timings: dict[str, float] | None = None,
) -> ShellResult:
    conflict = ConflictInfo(reason=reason, message=message)
    return ShellResult(
        success=False,
        exit_code=1,
        stdout="",
        stderr="",
        changed_paths=(),
        status="error",
        conflict=conflict,
        conflict_reason=message,
        warnings=(),
        timings=timings or {},
    )


def _overlay_cwd(cwd: str | None) -> str:
    if cwd is None or not str(cwd).strip():
        return "."
    return str(cwd)


__all__ = ["shell"]
