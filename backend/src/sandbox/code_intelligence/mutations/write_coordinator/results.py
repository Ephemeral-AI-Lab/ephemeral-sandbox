"""Result helpers for semantic write operations."""

from __future__ import annotations

from collections.abc import Sequence

from sandbox.code_intelligence.core.types import EditResult, OperationChange, OperationResult


def edit_result(
    file_path: str,
    message: str,
    *,
    success: bool = False,
    conflict: bool = False,
    conflict_reason: str = "",
    snapshot_id: str = "",
    timings: dict[str, float] | None = None,
) -> EditResult:
    return EditResult(
        success=success,
        file_path=file_path,
        message=message,
        conflict=conflict,
        conflict_reason=conflict_reason,
        snapshot_id=snapshot_id,
        timings=dict(timings or {}),
    )


def operation_abort(
    changes: Sequence[OperationChange],
    *,
    status: str,
    conflict_file: str | None,
    conflict_reason: str,
    timings: dict[str, float],
) -> OperationResult:
    is_conflict = status.startswith("aborted")
    files = tuple(
        edit_result(
            change.file_path,
            conflict_reason,
            conflict=is_conflict,
            conflict_reason=status if is_conflict else "",
        )
        for change in changes
    )
    return OperationResult(
        success=False,
        status=status,  # type: ignore[arg-type]
        files=files,
        conflict_file=conflict_file,
        conflict_reason=conflict_reason,
        timings=dict(timings),
    )
