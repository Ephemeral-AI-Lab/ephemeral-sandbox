from __future__ import annotations

import os

import pytest

from sandbox.daemon.handler.write import _write_out_of_workspace
from sandbox._shared.clock import monotonic_now


def test_write_out_of_workspace_refuses_terminal_symlink(tmp_path) -> None:
    target = tmp_path / "target.txt"
    target.write_text("existing\n", encoding="utf-8")
    link = tmp_path / "link.txt"
    os.symlink(target, link)

    with pytest.raises(ValueError, match="refusing to follow symlink"):
        _write_out_of_workspace(
            str(link),
            "replacement\n",
            overwrite=True,
            total_start=monotonic_now(),
        )

    assert target.read_text(encoding="utf-8") == "existing\n"
