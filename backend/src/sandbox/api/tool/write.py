"""Internal implementation for the public sandbox file-write verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool._daemon_requests import daemon_identity_payload
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox.api.tool._daemon_results import guarded_result_from_daemon_response
from sandbox.api.timeouts import WRITE_FILE_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_WRITE_FILE, SandboxTransport, call_sandbox_daemon
from sandbox._shared.models import WriteFileRequest, WriteFileResult


async def write_file(
    sandbox_id: str,
    request: WriteFileRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> WriteFileResult:
    """Write one UTF-8 file through sandbox-local OCC."""

    async def _call() -> WriteFileResult:
        payload = daemon_identity_payload(request) | {
            "path": request.path,
            "content": request.content,
            "description": request.default_description(f"write {request.path}"),
            "overwrite": request.overwrite,
        }
        response = await call_sandbox_daemon(
            sandbox_id,
            DAEMON_OP_WRITE_FILE,
            payload,
            timeout=WRITE_FILE_TIMEOUT_S,
            transport=transport,
        )
        return guarded_result_from_daemon_response(WriteFileResult, response)

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="write_file",
        caller=request.caller,
        payload={"path": request.path},
        call=_call,
    )


__all__ = ["write_file"]
