from __future__ import annotations

import os

import pytest

from sandbox.daemon.handler.edit import (
    _edit_out_of_workspace,
    edit_file,
)
from sandbox._shared.clock import monotonic_now


def test_edit_out_of_workspace_anchor_miss_returns_conflict(tmp_path) -> None:
    target = tmp_path / "probe.txt"
    target.write_text("alpha\n", encoding="utf-8")

    result = _edit_out_of_workspace(
        abs_path=str(target),
        edits=(("missing\n", "replacement\n", 1),),
        total_start=monotonic_now(),
    )

    assert result["success"] is False
    assert result["applied_edits"] == 0
    assert result["status"] == "aborted_overlap"
    assert result["changed_paths"] == [str(target)]
    assert result["conflict_reason"]
    conflict = result["conflict"]
    assert isinstance(conflict, dict)
    assert conflict["reason"] == "aborted_overlap"
    assert conflict["conflict_file"] == str(target)


def test_edit_out_of_workspace_expected_zero_with_absent_anchor_succeeds(
    tmp_path,
) -> None:
    """BL-05: ``expected_occurrences=0`` against an absent anchor must
    succeed (no replacements). The pre-fix truthy-coalesce silently rewrote
    the 0 to 1 and turned this into a conflict."""
    target = tmp_path / "probe.txt"
    original = "alpha\n"
    target.write_text(original, encoding="utf-8")

    result = _edit_out_of_workspace(
        abs_path=str(target),
        edits=(("missing\n", "replacement\n", 0),),
        total_start=monotonic_now(),
    )

    assert result["success"] is True
    assert result["applied_edits"] == 1  # one edit-spec applied (zero hits)
    assert result["changed_paths"] == [str(target)]
    # File contents unchanged because zero anchor hits → zero replacements.
    assert target.read_text(encoding="utf-8") == original


def test_edit_out_of_workspace_expected_zero_with_present_anchor_rejects(
    tmp_path,
) -> None:
    """BL-05: ``expected_occurrences=0`` against a present anchor must
    reject because ``found (1) != expected (0)``."""
    target = tmp_path / "probe.txt"
    target.write_text("alpha\n", encoding="utf-8")

    result = _edit_out_of_workspace(
        abs_path=str(target),
        edits=(("alpha\n", "beta\n", 0),),
        total_start=monotonic_now(),
    )

    assert result["success"] is False
    assert result["status"] == "aborted_overlap"
    assert result["conflict_reason"]
    # File unchanged on conflict.
    assert target.read_text(encoding="utf-8") == "alpha\n"


def test_edit_out_of_workspace_refuses_terminal_symlink(tmp_path) -> None:
    target = tmp_path / "target.txt"
    target.write_text("alpha\n", encoding="utf-8")
    link = tmp_path / "link.txt"
    os.symlink(target, link)

    with pytest.raises(ValueError, match="refusing to follow symlink"):
        _edit_out_of_workspace(
            abs_path=str(link),
            edits=(("alpha", "beta", 1),),
            total_start=monotonic_now(),
        )

    assert target.read_text(encoding="utf-8") == "alpha\n"


async def test_edit_file_rejects_negative_expected_occurrences() -> None:
    """BL-05: negative ``expected_occurrences`` must be rejected at parse
    time rather than coerced into bogus downstream behaviour."""
    with pytest.raises(ValueError, match="expected_occurrences must be >= 0"):
        await edit_file(
            {
                "path": "/tmp/anything",
                "edits": [
                    {
                        "old_text": "a",
                        "new_text": "b",
                        "expected_occurrences": -1,
                    }
                ],
            }
        )
