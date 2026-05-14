"""Internal implementation for the public sandbox file-write verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api._impl._audit import audited_operation
from sandbox.api._impl._results import guarded_result_from_payload
from sandbox.api.protocol import SandboxTransport
from sandbox.api.timeouts import WRITE_FILE_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_WRITE_FILE, DaemonSandboxTransport
from sandbox.models import WriteFileRequest, WriteFileResult


async def write_file(
    sandbox_id: str,
    request: WriteFileRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> WriteFileResult:
    """Write one UTF-8 file through sandbox-local OCC."""
    selected_transport = transport or DaemonSandboxTransport()

    async def _call() -> WriteFileResult:
        raw = await selected_transport.call(
            sandbox_id,
            DAEMON_OP_WRITE_FILE,
            {
                "path": request.path,
                "content": request.content,
                "actor_id": request.caller.agent_id,
                "caller": request.caller.audit_fields(),
                "description": request.default_description(f"write {request.path}"),
                "overwrite": request.overwrite,
            },
            timeout=WRITE_FILE_TIMEOUT_S,
        )
        return guarded_result_from_payload(WriteFileResult, raw)

    return await audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="write_file",
        caller=request.caller,
        payload={"path": request.path},
        call=_call,
    )


__all__ = ["write_file"]
