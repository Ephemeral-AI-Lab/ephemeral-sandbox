"""Internal implementation for the public sandbox file-write verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api._impl._audit import audited_operation
from sandbox.api._impl._payload import caller_audit_fields
from sandbox.api._impl._recovery import call_with_transient_recovery
from sandbox.api._impl._results import write_result_from_payload
from sandbox.api.protocol import SandboxTransport
from sandbox.api.timeouts import (
    RECOVERY_READ_TIMEOUT_S,
    TRANSIENT_MUTATION_ATTEMPTS,
    WRITE_FILE_TIMEOUT_S,
)
from sandbox.api.transport import (
    DAEMON_OP_READ_FILE,
    DAEMON_OP_WRITE_FILE,
    DaemonSandboxTransport,
)
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
        raw = await _call_write_with_recovery(
            sandbox_id,
            request,
            transport=selected_transport,
        )
        return write_result_from_payload(raw)

    return await audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="write_file",
        caller=request.caller,
        payload={"path": request.path},
        call=_call,
    )


def _write_payload(request: WriteFileRequest) -> dict[str, object]:
    return {
        "path": request.path,
        "content": request.content,
        "actor_id": request.caller.agent_id,
        "caller": caller_audit_fields(request.caller),
        "description": request.default_description(f"write {request.path}"),
        "overwrite": request.overwrite,
    }


async def _call_write_with_recovery(
    sandbox_id: str,
    request: WriteFileRequest,
    *,
    transport: SandboxTransport,
) -> dict[str, object]:
    payload = _write_payload(request)
    recovery_can_be_proven = await _write_recovery_can_be_proven(
        sandbox_id,
        request,
        transport=transport,
    )

    async def _call() -> dict[str, object]:
        return await transport.call(
            sandbox_id,
            DAEMON_OP_WRITE_FILE,
            payload,
            timeout=WRITE_FILE_TIMEOUT_S,
        )

    async def _recover(attempt_no: int) -> dict[str, object] | None:
        return await _recover_if_write_already_applied(
            sandbox_id,
            request,
            recovery_can_be_proven=recovery_can_be_proven,
            attempt_no=attempt_no,
            transport=transport,
        )

    return await call_with_transient_recovery(
        attempts=TRANSIENT_MUTATION_ATTEMPTS,
        call=_call,
        recover=_recover,
    )


async def _write_recovery_can_be_proven(
    sandbox_id: str,
    request: WriteFileRequest,
    *,
    transport: SandboxTransport,
) -> bool:
    try:
        raw = await transport.call(
            sandbox_id,
            DAEMON_OP_READ_FILE,
            {
                "path": request.path,
                "caller": caller_audit_fields(request.caller),
            },
            timeout=RECOVERY_READ_TIMEOUT_S,
        )
    except Exception:
        return False
    if not raw.get("success"):
        return False
    if not raw.get("exists"):
        return True
    return str(raw.get("content", "")) != request.content


async def _recover_if_write_already_applied(
    sandbox_id: str,
    request: WriteFileRequest,
    *,
    recovery_can_be_proven: bool,
    attempt_no: int,
    transport: SandboxTransport,
) -> dict[str, object] | None:
    if not recovery_can_be_proven:
        return None
    try:
        raw = await transport.call(
            sandbox_id,
            DAEMON_OP_READ_FILE,
            {
                "path": request.path,
                "caller": caller_audit_fields(request.caller),
            },
            timeout=RECOVERY_READ_TIMEOUT_S,
        )
    except Exception:
        return None
    if not raw.get("success") or not raw.get("exists"):
        return None
    if str(raw.get("content", "")) != request.content:
        return None
    return {
        "success": True,
        "changed_paths": [request.path],
        "status": "written",
        "conflict": None,
        "conflict_reason": None,
        "timings": {
            "api.write.recovered_after_transient": 1.0,
            "api.write.recovery_attempt": float(attempt_no),
        },
    }


__all__ = ["write_file"]
