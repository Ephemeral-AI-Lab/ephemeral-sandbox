"""Public sandbox file-edit verb."""

from __future__ import annotations

from collections.abc import Sequence

from sandbox.api.models import ConflictInfo, EditFileRequest, EditFileResult
from sandbox.occ.changeset.types import (
    ChangesetResult,
    EditChange,
    FileResult,
    FileStatus,
)
from sandbox.occ.client import OCCClient
from sandbox.occ.patching.patcher import SearchReplaceEdit as OCCSearchReplaceEdit


async def edit_file(sandbox_id: str, request: EditFileRequest) -> EditFileResult:
    """Apply search/replace edits through the OCC runtime peer."""
    change = EditChange(
        path=request.path,
        edits=tuple(
            OCCSearchReplaceEdit(old_text=edit.old_text, new_text=edit.new_text)
            for edit in request.edits
        ),
    )
    result = await OCCClient(sandbox_id).apply_changeset(
        [change],
        agent_id=request.actor.agent_id,
        description=request.description or f"edit {request.path}",
    )
    return _result_from_changeset(
        result,
        fallback_path=request.path,
        edit_count=len(request.edits),
    )


def _result_from_changeset(
    result: ChangesetResult,
    *,
    fallback_path: str,
    edit_count: int,
) -> EditFileResult:
    changed_paths = _committed_paths(result.files, fallback_path=fallback_path)
    conflict, status = _conflict_and_status(result.files)
    return EditFileResult(
        success=result.success,
        changed_paths=changed_paths,
        applied_edits=edit_count if result.success else 0,
        status=status,
        conflict=conflict,
        conflict_reason=conflict.message if conflict is not None else None,
    )


def _committed_paths(
    files: Sequence[FileResult],
    *,
    fallback_path: str,
) -> tuple[str, ...]:
    committed = tuple(f.path for f in files if f.status is FileStatus.COMMITTED and f.path)
    if committed:
        return committed
    aborted = next((f for f in files if f.status is not FileStatus.COMMITTED and f.path), None)
    if aborted is not None:
        return (aborted.path,)
    return (fallback_path,) if not files else ()


def _conflict_and_status(
    files: Sequence[FileResult],
) -> tuple[ConflictInfo | None, str]:
    if not files:
        return None, "committed"
    bad = next((f for f in files if f.status is not FileStatus.COMMITTED), None)
    if bad is None:
        return None, "committed"
    status = bad.status.value
    return (
        ConflictInfo(
            reason=status,
            conflict_file=bad.path or None,
            message=bad.message or status,
        ),
        status,
    )


__all__ = ["edit_file"]
