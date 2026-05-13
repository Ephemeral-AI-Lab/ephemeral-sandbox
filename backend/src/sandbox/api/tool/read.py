"""Public sandbox file-read verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.audit.operation import (
    publish_operation_failed,
    publish_operation_result,
    publish_operation_started,
)
from sandbox.api.tool._payload import caller_envelope, timings_from_payload
from sandbox.models import ReadFileRequest, ReadFileResult
from sandbox.host.daemon_client import call_daemon_api


async def read_file(
    sandbox_id: str,
    request: ReadFileRequest,
    *,
    audit_sink: AuditSink | None = None,
) -> ReadFileResult:
    """Read one UTF-8 text file through the sandbox daemon."""
    publish_operation_started(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="read_file",
        caller=request.caller,
        payload={"path": request.path},
    )
    try:
        raw = await call_daemon_api(
            sandbox_id,
            "api.read_file",
            {
                "path": request.path,
                "caller": caller_envelope(request.caller),
            },
            timeout=60,
        )
        result = ReadFileResult(
            success=bool(raw.get("success", False)),
            exists=bool(raw.get("exists", False)),
            content=str(raw.get("content", "")),
            encoding=str(raw.get("encoding", "utf-8")),
            timings=timings_from_payload(raw.get("timings")),
        )
    except Exception as exc:
        publish_operation_failed(
            audit_sink,
            sandbox_id=sandbox_id,
            operation="read_file",
            caller=request.caller,
            error=exc,
        )
        raise
    publish_operation_result(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="read_file",
        caller=request.caller,
        result=result,
    )
    return result


__all__ = ["read_file"]
