"""Public sandbox file-write verb."""

from __future__ import annotations

from sandbox.api.models import ConflictInfo, WriteFileRequest, WriteFileResult
from sandbox.occ.client import OCCClient
from sandbox.occ.types import OperationResult, WriteSpec


async def write_file(sandbox_id: str, request: WriteFileRequest) -> WriteFileResult:
    """Write one UTF-8 file through the OCC runtime peer."""
    operation = await OCCClient(sandbox_id).write(
        WriteSpec(
            file_path=request.path,
            content=request.content,
            overwrite=request.overwrite,
        ),
        agent_id=request.actor.agent_id,
        description=request.description or f"write {request.path}",
    )
    return _result_from_operation(operation, fallback_path=request.path)


def _result_from_operation(
    operation: OperationResult,
    *,
    fallback_path: str,
) -> WriteFileResult:
    changed_paths = _changed_paths(operation, fallback_path=fallback_path)
    conflict = _conflict_from_operation(operation)
    return WriteFileResult(
        success=operation.success,
        changed_paths=changed_paths,
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


__all__ = ["write_file"]
