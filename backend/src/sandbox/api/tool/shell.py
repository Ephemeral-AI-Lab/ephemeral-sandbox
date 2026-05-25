"""Internal implementation for the public sandbox shell verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool._conflict_detection import is_shell_conflict
from sandbox.api.tool._daemon_requests import daemon_identity_payload
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox.api.tool._daemon_results import (
    shell_result_from_daemon_response,
    timing_map_from_daemon_field,
    user_visible_error_message,
)
from sandbox.api.timeouts import shell_dispatch_timeout
from sandbox.api.transport import DAEMON_OP_SHELL, SandboxTransport, call_sandbox_daemon
from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import ConflictInfo, ShellRequest, ShellResult


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

    async def _call() -> ShellResult:
        if request.stdin is not None:
            message = "snapshot overlay shell does not accept stdin"
            return ShellResult(
                success=False,
                exit_code=1,
                stdout="",
                stderr="",
                changed_paths=(),
                status="error",
                conflict=ConflictInfo.rejected(
                    reason="stdin_not_supported",
                    message=message,
                ),
                conflict_reason=message,
                warnings=(),
                timings={"api.shell.total_s": monotonic_now() - total_start},
            )
        payload = daemon_identity_payload(request) | {
            "command": request.command,
            "cwd": cwd,
            "timeout_seconds": request.timeout,
            "description": request.default_description("shell"),
        }
        if request.background:
            payload["background"] = True
        response = await call_sandbox_daemon(
            sandbox_id,
            DAEMON_OP_SHELL,
            payload,
            timeout=shell_dispatch_timeout(request.timeout),
            transport=transport,
        )
        timings = timing_map_from_daemon_field(response.get("timings"))
        timings["api.shell.dispatch_total_s"] = monotonic_now() - total_start
        return shell_result_from_daemon_response(response, timings=timings)

    def _conflict_from_error(exc: BaseException) -> ShellResult | None:
        if not is_shell_conflict(exc):
            return None
        message = user_visible_error_message(exc)
        return ShellResult(
            success=False,
            exit_code=1,
            stdout="",
            stderr="",
            changed_paths=(),
            status="rejected",
            conflict=ConflictInfo.rejected(message=message),
            conflict_reason=message,
            warnings=(),
            timings={"api.shell.dispatch_total_s": monotonic_now() - total_start},
        )

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="shell",
        caller=request.caller,
        payload={"cwd": cwd},
        call=_call,
        conflict_from_error=_conflict_from_error,
    )


__all__ = ["shell"]
