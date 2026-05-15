"""Internal implementation for the public sandbox file-read verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api._impl._audit import audited_operation
from sandbox.api._impl._results import read_result_from_payload
from sandbox.api.protocol import SandboxTransport
from sandbox.api.timeouts import READ_FILE_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_READ_FILE, DaemonSandboxTransport
from sandbox._shared.models import ReadFileRequest, ReadFileResult


async def read_file(
    sandbox_id: str,
    request: ReadFileRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> ReadFileResult:
    """Read one UTF-8 text file through the sandbox daemon."""
    selected_transport = transport or DaemonSandboxTransport()

    async def _call() -> ReadFileResult:
        raw = await selected_transport.call(
            sandbox_id,
            DAEMON_OP_READ_FILE,
            {"path": request.path, "caller": request.caller.audit_fields()},
            timeout=READ_FILE_TIMEOUT_S,
        )
        return read_result_from_payload(raw)

    return await audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="read_file",
        caller=request.caller,
        payload={"path": request.path},
        call=_call,
    )


__all__ = ["read_file"]
