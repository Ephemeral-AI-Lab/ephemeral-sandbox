"""Internal implementation for the public sandbox file-edit verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool._conflict_detection import is_edit_conflict
from sandbox.api.tool._daemon_response_parsing import (
    daemon_request_identity_fields,
    parse_guarded_mutation_result,
    strict_int_from_daemon_field,
    user_visible_error_message,
)
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox.api.timeouts import EDIT_FILE_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_EDIT_FILE, SandboxTransport, call_sandbox_daemon
from sandbox._shared.models import ConflictInfo, EditFileRequest, EditFileResult


async def edit_file(
    sandbox_id: str,
    request: EditFileRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> EditFileResult:
    """Apply search/replace edits through sandbox-local OCC."""

    async def _call() -> EditFileResult:
        payload = daemon_request_identity_fields(request) | {
            "path": request.path,
            "edits": [
                {
                    "old_text": edit.old_text,
                    "new_text": edit.new_text,
                    "replace_all": edit.replace_all,
                }
                for edit in request.edits
            ],
            "description": request.default_description(f"edit {request.path}"),
        }
        response = await call_sandbox_daemon(
            sandbox_id,
            DAEMON_OP_EDIT_FILE,
            payload,
            timeout=EDIT_FILE_TIMEOUT_S,
            transport=transport,
        )
        return parse_guarded_mutation_result(
            EditFileResult,
            response,
            applied_edits=strict_int_from_daemon_field(
                response.get("applied_edits"), default=0
            ),
        )

    def _conflict_from_error(exc: BaseException) -> EditFileResult | None:
        if not is_edit_conflict(exc):
            return None
        message = user_visible_error_message(exc)
        return EditFileResult(
            success=False,
            changed_paths=(request.path,),
            applied_edits=0,
            status="aborted_overlap",
            conflict=ConflictInfo.overlap(path=request.path, message=message),
            conflict_reason=message,
            timings={},
        )

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="edit_file",
        caller=request.caller,
        payload={"path": request.path},
        call=_call,
        conflict_from_error=_conflict_from_error,
    )


__all__ = ["edit_file"]
