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
