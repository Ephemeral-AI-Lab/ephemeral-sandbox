"""Internal implementation for the public sandbox shell verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api._impl._audit import audited_operation
from sandbox.api._impl._classifiers import is_shell_conflict
from sandbox.api._impl._payload import (
    error_message,
    normalize_overlay_cwd,
    timings_from_payload,
)
from sandbox.api._impl._results import (
    shell_conflict_result,
    shell_error_result,
    shell_result_from_payload,
)
from sandbox.api.protocol import SandboxTransport
from sandbox.api.timeouts import shell_dispatch_timeout
from sandbox.api.transport import DAEMON_OP_SHELL, DaemonSandboxTransport
from sandbox.models import ShellRequest, ShellResult
from sandbox.timing import monotonic_now


async def shell(
    sandbox_id: str,
    request: ShellRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> ShellResult:
    """Run a shell command through sandbox-local overlay and OCC."""
    total_start = monotonic_now()
    cwd = normalize_overlay_cwd(request.cwd)
    selected_transport = transport or DaemonSandboxTransport()

    async def _call() -> ShellResult:
        if request.stdin is not None:
            return shell_error_result(
                reason="stdin_not_supported",
                message="snapshot overlay shell does not accept stdin",
                timings={"api.shell.total_s": monotonic_now() - total_start},
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
        timings = timings_from_payload(raw.get("timings"))
        timings["api.shell.dispatch_total_s"] = monotonic_now() - total_start
        return shell_result_from_payload(raw, timings=timings)

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
        payload={"cwd": cwd},
        call=_call,
        conflict_from_error=_conflict_from_error,
    )


__all__ = ["shell"]
