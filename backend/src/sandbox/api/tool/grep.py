"""Internal implementation for the public sandbox grep verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool._daemon_requests import daemon_identity_payload
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox.api.tool._daemon_results import grep_result_from_daemon_response
from sandbox.api.timeouts import GREP_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_GREP, SandboxTransport, call_sandbox_daemon
from sandbox._shared.models import GrepRequest, GrepResult


async def grep(
    sandbox_id: str,
    request: GrepRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> GrepResult:
    """Regex-scan workspace file contents under the sandbox's leased snapshot."""

    async def _call() -> GrepResult:
        payload = daemon_identity_payload(request) | {
            "pattern": request.pattern,
            "output_mode": request.output_mode,
            "offset": request.offset,
            "case_insensitive": request.case_insensitive,
            "line_numbers": request.line_numbers,
            "multiline": request.multiline,
        }
        if request.path is not None:
            payload["path"] = request.path
        if request.glob_filter is not None:
            payload["glob_filter"] = request.glob_filter
        if request.head_limit is not None:
            payload["head_limit"] = request.head_limit
        response = await call_sandbox_daemon(
            sandbox_id,
            DAEMON_OP_GREP,
            payload,
            timeout=GREP_TIMEOUT_S,
            transport=transport,
        )
        return grep_result_from_daemon_response(response)

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="grep",
        caller=request.caller,
        payload={
            "pattern": request.pattern,
            "path": request.path or "",
            "output_mode": request.output_mode,
        },
        call=_call,
    )


__all__ = ["grep"]
