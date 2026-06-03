from __future__ import annotations

import os

import pytest

from sandbox._shared.tool_primitives.write import write_file


def test_write_out_of_workspace_refuses_terminal_symlink(tmp_path) -> None:
    target = tmp_path / "target.txt"
    target.write_text("existing\n", encoding="utf-8")
    link = tmp_path / "link.txt"
    os.symlink(target, link)

    with pytest.raises(ValueError, match="refusing to follow symlink"):
        write_file(
            {
                "path": str(link),
                "content": "replacement\n",
                "overwrite": True,
            }
        )

    assert target.read_text(encoding="utf-8") == "existing\n"
