"""Public sandbox file-write verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.audit.operation import (
    publish_operation_failed,
    publish_operation_result,
    publish_operation_started,
)
from sandbox.api.tool._payload import (
    caller_envelope,
    conflict_from_payload,
    paths_from_payload,
    timings_from_payload,
)
from sandbox.models import WriteFileRequest, WriteFileResult
from sandbox.host.daemon_client import call_daemon_api


async def write_file(
    sandbox_id: str,
    request: WriteFileRequest,
    *,
    audit_sink: AuditSink | None = None,
) -> WriteFileResult:
    """Write one UTF-8 file through sandbox-local OCC."""
    publish_operation_started(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="write_file",
        caller=request.caller,
        payload={"path": request.path},
    )
    try:
        raw = await call_daemon_api(
            sandbox_id,
            "api.write_file",
            {
                "path": request.path,
                "content": request.content,
                "actor_id": request.caller.agent_id,
                "caller": caller_envelope(request.caller),
                "description": request.description or f"write {request.path}",
                "overwrite": request.overwrite,
            },
            timeout=60,
        )
        conflict = conflict_from_payload(raw.get("conflict"))
        result = WriteFileResult(
            success=bool(raw.get("success", False)),
            changed_paths=paths_from_payload(raw.get("changed_paths")),
            status=str(raw.get("status", "")),
            conflict=conflict,
            conflict_reason=(
                str(raw.get("conflict_reason"))
                if raw.get("conflict_reason") is not None
                else None
            ),
            timings=timings_from_payload(raw.get("timings")),
        )
    except Exception as exc:
        publish_operation_failed(
            audit_sink,
            sandbox_id=sandbox_id,
            operation="write_file",
            caller=request.caller,
            error=exc,
        )
        raise
    publish_operation_result(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="write_file",
        caller=request.caller,
        result=result,
    )
    return result


__all__ = ["write_file"]
