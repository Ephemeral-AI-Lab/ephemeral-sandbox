"""Regression tests for guarded sandbox API result status."""

from __future__ import annotations

from sandbox.api.edit import _conflict_from_operation as edit_conflict_from_operation
from sandbox.api.write import _result_from_operation
from sandbox.occ.types import OperationResult


def test_write_result_preserves_status_and_human_conflict_reason() -> None:
    result = _result_from_operation(
        OperationResult(
            success=False,
            status="aborted_overlap",
            conflict_file="/ws/app.py",
            conflict_reason="concurrent edit overlaps the operation window",
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
    conflict = edit_conflict_from_operation(
        OperationResult(
            success=False,
            status="aborted_version",
            conflict_file="/ws/app.py",
            conflict_reason="file content changed before delete",
        )
    )

    assert conflict is not None
    assert conflict.reason == "aborted_version"
    assert conflict.conflict_file == "/ws/app.py"
    assert conflict.message == "file content changed before delete"
