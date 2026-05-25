"""Internal implementation for the public sandbox file-read verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool._daemon_requests import daemon_identity_payload
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox.api.tool._daemon_results import read_result_from_daemon_response
from sandbox.api.timeouts import READ_FILE_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_READ_FILE, SandboxTransport, call_sandbox_daemon
from sandbox._shared.models import ReadFileRequest, ReadFileResult


async def read_file(
    sandbox_id: str,
    request: ReadFileRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> ReadFileResult:
    """Read one UTF-8 text file through the sandbox daemon."""

    async def _call() -> ReadFileResult:
        payload = daemon_identity_payload(request) | {"path": request.path}
        response = await call_sandbox_daemon(
            sandbox_id,
            DAEMON_OP_READ_FILE,
            payload,
            timeout=READ_FILE_TIMEOUT_S,
            transport=transport,
        )
        return read_result_from_daemon_response(response)

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="read_file",
        caller=request.caller,
        payload={"path": request.path},
        call=_call,
    )


__all__ = ["read_file"]
