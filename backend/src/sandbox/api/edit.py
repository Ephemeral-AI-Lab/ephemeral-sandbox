"""Public sandbox file-edit verb."""

from __future__ import annotations

from sandbox.api.models import ConflictInfo, EditFileRequest, EditFileResult
from sandbox.occ.client import OCCClient
from sandbox.occ.patching.patcher import SearchReplaceEdit as OCCSearchReplaceEdit
from sandbox.occ.types import EditSpec, OperationResult


async def edit_file(sandbox_id: str, request: EditFileRequest) -> EditFileResult:
    """Apply search/replace edits through the OCC runtime peer."""
    operation = await OCCClient(sandbox_id).edit(
        EditSpec(
            file_path=request.path,
            edits=tuple(
                OCCSearchReplaceEdit(old_text=edit.old_text, new_text=edit.new_text)
                for edit in request.edits
            ),
        ),
        agent_id=request.actor.agent_id,
        description=request.description or f"edit {request.path}",
    )
    conflict = _conflict_from_operation(operation)
    return EditFileResult(
        success=operation.success,
        changed_paths=_changed_paths(operation, fallback_path=request.path),
        applied_edits=len(request.edits) if operation.success else 0,
        status=operation.status,
        conflict=conflict,
        conflict_reason=conflict.message if conflict is not None else None,
    )


def _changed_paths(
    operation: OperationResult,
    *,
    fallback_path: str,
) -> tuple[str, ...]:
    paths = tuple(file.file_path for file in operation.files if file.file_path)
    if paths:
        return paths
    if operation.conflict_file:
        return (operation.conflict_file,)
    return (fallback_path,) if operation.success else ()


def _conflict_from_operation(operation: OperationResult) -> ConflictInfo | None:
    if operation.success:
        return None
    reason = operation.status or "failed"
    message = operation.conflict_reason or reason
    return ConflictInfo(
        reason=reason,
        conflict_file=operation.conflict_file,
        message=message,
    )


__all__ = ["edit_file"]
