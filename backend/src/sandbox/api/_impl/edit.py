"""Internal implementation for the public sandbox file-edit verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api._impl._audit import audited_operation
from sandbox.api._impl._classifiers import is_edit_conflict
from sandbox.api._impl._payload import (
    caller_audit_fields,
    error_message,
)
from sandbox.api._impl._recovery import call_with_transient_recovery
from sandbox.api._impl._results import edit_conflict_result, edit_result_from_payload
from sandbox.api.protocol import SandboxTransport
from sandbox.api.timeouts import (
    EDIT_FILE_TIMEOUT_S,
    RECOVERY_READ_TIMEOUT_S,
    TRANSIENT_MUTATION_ATTEMPTS,
)
from sandbox.api.transport import (
    DAEMON_OP_EDIT_FILE,
    DAEMON_OP_READ_FILE,
    DaemonSandboxTransport,
)
from sandbox.models import EditFileRequest, EditFileResult


async def edit_file(
    sandbox_id: str,
    request: EditFileRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> EditFileResult:
    """Apply search/replace edits through sandbox-local OCC."""
    selected_transport = transport or DaemonSandboxTransport()

    async def _call() -> EditFileResult:
        raw = await _call_edit_with_recovery(
            sandbox_id,
            request,
            transport=selected_transport,
        )
        return edit_result_from_payload(raw)

    return await audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="edit_file",
        caller=request.caller,
        payload={"path": request.path},
        call=_call,
        conflict_from_error=lambda exc: _conflict_result_from_error(request.path, exc),
    )


async def _call_edit_with_recovery(
    sandbox_id: str,
    request: EditFileRequest,
    *,
    transport: SandboxTransport,
) -> dict[str, object]:
    payload = _edit_payload(request)
    expected_content = await _expected_content_after_edit(
        sandbox_id,
        request,
        transport=transport,
    )

    async def _call() -> dict[str, object]:
        return await transport.call(
            sandbox_id,
            DAEMON_OP_EDIT_FILE,
            payload,
            timeout=EDIT_FILE_TIMEOUT_S,
        )

    async def _recover(attempt_no: int) -> dict[str, object] | None:
        return await _recover_if_edit_already_applied(
            sandbox_id,
            request,
            expected_content=expected_content,
            attempt_no=attempt_no,
            transport=transport,
        )

    return await call_with_transient_recovery(
        attempts=TRANSIENT_MUTATION_ATTEMPTS,
        call=_call,
        recover=_recover,
    )


def _edit_payload(request: EditFileRequest) -> dict[str, object]:
    return {
        "path": request.path,
        "edits": [
            {"old_text": edit.old_text, "new_text": edit.new_text}
            for edit in request.edits
        ],
        "actor_id": request.caller.agent_id,
        "caller": caller_audit_fields(request.caller),
        "description": request.default_description(f"edit {request.path}"),
    }


async def _expected_content_after_edit(
    sandbox_id: str,
    request: EditFileRequest,
    *,
    transport: SandboxTransport,
) -> str | None:
    if not request.edits:
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
    content = str(raw.get("content", ""))
    for edit in request.edits:
        if not edit.old_text or content.count(edit.old_text) != 1:
            return None
        content = content.replace(edit.old_text, edit.new_text, 1)
    return content


async def _recover_if_edit_already_applied(
    sandbox_id: str,
    request: EditFileRequest,
    *,
    expected_content: str | None,
    attempt_no: int,
    transport: SandboxTransport,
) -> dict[str, object] | None:
    if expected_content is None:
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
    content = str(raw.get("content", ""))
    if content != expected_content:
        return None
    return {
        "success": True,
        "changed_paths": [request.path],
        "applied_edits": len(request.edits),
        "status": "edited",
        "conflict": None,
        "conflict_reason": None,
        "timings": {
            "api.edit.recovered_after_transient": 1.0,
            "api.edit.recovery_attempt": float(attempt_no),
        },
    }


def _conflict_result_from_error(path: str, error: BaseException) -> EditFileResult | None:
    if not is_edit_conflict(error):
        return None
    return edit_conflict_result(path, error_message(error))


__all__ = ["edit_file"]
