"""Regression tests for guarded sandbox API result status."""

from __future__ import annotations

from sandbox.api.edit import _conflict_and_status as edit_conflict_and_status
from sandbox.api.write import _result_from_changeset
from sandbox.occ.changeset.types import (
    ChangesetResult,
    FileResult,
    FileStatus,
)


def test_write_result_preserves_status_and_human_conflict_reason() -> None:
    result = _result_from_changeset(
        ChangesetResult(
            files=(
                FileResult(
                    path="/ws/app.py",
                    status=FileStatus.ABORTED_OVERLAP,
                    message="concurrent edit overlaps the operation window",
                ),
            ),
        ),
        fallback_path="/ws/app.py",
    )

    assert result.status == "aborted_overlap"
    assert result.changed_paths == ("/ws/app.py",)
    assert result.conflict_reason == "concurrent edit overlaps the operation window"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_overlap"
    assert result.conflict.message == "concurrent edit overlaps the operation window"


def test_edit_conflict_uses_status_as_reason_and_message_as_detail() -> None:
    conflict, status = edit_conflict_and_status(
        (
            FileResult(
                path="/ws/app.py",
                status=FileStatus.ABORTED_VERSION,
                message="file content changed before delete",
            ),
        )
    )

    assert status == "aborted_version"
    assert conflict is not None
    assert conflict.reason == "aborted_version"
    assert conflict.conflict_file == "/ws/app.py"
    assert conflict.message == "file content changed before delete"
