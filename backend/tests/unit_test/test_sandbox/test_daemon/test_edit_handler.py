from __future__ import annotations

import os

import pytest

from sandbox._shared.tool_primitives.edit import edit_file


def test_edit_anchor_miss_raises_without_writing(tmp_path) -> None:
    target = tmp_path / "probe.txt"
    target.write_text("alpha\n", encoding="utf-8")

    with pytest.raises(ValueError, match="anchor not found"):
        edit_file(
            {
                "path": str(target),
                "edits": [{"old_text": "missing\n", "new_text": "replacement\n"}],
            }
        )

    assert target.read_text(encoding="utf-8") == "alpha\n"


def test_edit_out_of_workspace_expected_zero_with_absent_anchor_succeeds(
    tmp_path,
) -> None:
    """BL-05: ``expected_occurrences=0`` against an absent anchor must
    succeed (no replacements). The pre-fix truthy-coalesce silently rewrote
    the 0 to 1 and turned this into a conflict."""
    target = tmp_path / "probe.txt"
    original = "alpha\n"
    target.write_text(original, encoding="utf-8")

    result = edit_file(
        {
            "path": str(target),
            "edits": [
                {
                    "old_text": "missing\n",
                    "new_text": "replacement\n",
                    "expected_occurrences": 0,
                }
            ],
        }
    )

    assert result.success is True
    assert result.applied_edits == 1  # one edit-spec applied (zero hits)
    assert result.changed_paths == (str(target),)
    # File contents unchanged because zero anchor hits → zero replacements.
    assert target.read_text(encoding="utf-8") == original


def test_edit_out_of_workspace_expected_zero_with_present_anchor_rejects(
    tmp_path,
) -> None:
    """BL-05: ``expected_occurrences=0`` against a present anchor must
    reject because ``found (1) != expected (0)``."""
    target = tmp_path / "probe.txt"
    target.write_text("alpha\n", encoding="utf-8")

    with pytest.raises(ValueError, match="expected 0 occurrences"):
        edit_file(
            {
                "path": str(target),
                "edits": [
                    {
                        "old_text": "alpha\n",
                        "new_text": "beta\n",
                        "expected_occurrences": 0,
                    }
                ],
            }
        )

    assert target.read_text(encoding="utf-8") == "alpha\n"


def test_edit_out_of_workspace_refuses_terminal_symlink(tmp_path) -> None:
    target = tmp_path / "target.txt"
    target.write_text("alpha\n", encoding="utf-8")
    link = tmp_path / "link.txt"
    os.symlink(target, link)

    with pytest.raises(ValueError, match="refusing to follow symlink"):
        edit_file(
            {
                "path": str(link),
                "edits": [{"old_text": "alpha", "new_text": "beta"}],
            }
        )

    assert target.read_text(encoding="utf-8") == "alpha\n"


async def test_edit_file_rejects_negative_expected_occurrences(tmp_path) -> None:
    """BL-05: negative ``expected_occurrences`` must be rejected at parse
    time rather than coerced into bogus downstream behaviour."""
    target = tmp_path / "probe.txt"
    target.write_text("alpha\n", encoding="utf-8")
    with pytest.raises(ValueError, match="expected_occurrences must be >= 0"):
        edit_file(
            {
                "path": str(target),
                "edits": [
                    {
                        "old_text": "a",
                        "new_text": "b",
                        "expected_occurrences": -1,
                    }
                ],
            }
        )
