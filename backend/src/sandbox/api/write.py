"""Public sandbox file-write verb."""

from __future__ import annotations

from collections.abc import Sequence

from sandbox.api.models import ConflictInfo, WriteFileRequest, WriteFileResult
from sandbox.occ.changeset.types import (
    ChangesetResult,
    FileResult,
    FileStatus,
    WriteChange,
)
from sandbox.occ.client import OCCClient


async def write_file(sandbox_id: str, request: WriteFileRequest) -> WriteFileResult:
    """Write one UTF-8 file through the OCC runtime peer.

    The host does not read the file's current base content before sending the
    change; the gate's per-file lock guards the write atomically. This is the
    "blind write under per-file lock" mode documented in
    ``.omc/plans/occ-changeset-gate-simplification.md`` §"How base_hash is
    obtained" and matches the existing ``write_file`` semantics.
    """
    change = WriteChange(
        path=request.path,
        base_hash="",
        # base_existed=False encodes "create only" (overwrite=False); True is
        # interpreted as "blind write under lock" since base_hash is empty.
        base_existed=request.overwrite,
        final_content=request.content,
    )
    result = await OCCClient(sandbox_id).apply_changeset(
        [change],
        agent_id=request.actor.agent_id,
        description=request.description or f"write {request.path}",
    )
    return _result_from_changeset(result, fallback_path=request.path)


def _result_from_changeset(
    result: ChangesetResult,
    *,
    fallback_path: str,
) -> WriteFileResult:
    changed_paths = _committed_paths(result.files, fallback_path=fallback_path)
    conflict, status = _conflict_and_status(result.files)
    return WriteFileResult(
        success=result.success,
        changed_paths=changed_paths,
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


__all__ = ["write_file"]
