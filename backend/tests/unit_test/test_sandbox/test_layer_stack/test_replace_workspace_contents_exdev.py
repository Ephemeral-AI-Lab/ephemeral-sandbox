"""``_replace_workspace_contents`` must fall back to shutil.move on EXDEV.

Docker bind-mounts ``/testbed`` as a separate device, so a raw
``os.replace`` across the boundary raises ``OSError(EXDEV)`` and the
private helper has to retry with ``shutil.move``.
"""

from __future__ import annotations

import errno
import os
from pathlib import Path
from unittest.mock import patch

import pytest

from sandbox.layer_stack.stack import _replace_workspace_contents


def test_replace_workspace_contents_falls_back_to_shutil_move_on_exdev(
    tmp_path: Path,
) -> None:
    destination = tmp_path / "dst"
    source = tmp_path / "src"
    destination.mkdir()
    source.mkdir()
    (source / "alpha.txt").write_text("alpha", encoding="utf-8")
    (source / "beta").mkdir()
    (source / "beta" / "inner.txt").write_text("beta-inner", encoding="utf-8")

    real_replace = os.replace
    moved: list[tuple[str, str]] = []

    def replace_raises_exdev(src: object, dst: object) -> None:
        raise OSError(errno.EXDEV, "Invalid cross-device link")

    import shutil as _shutil

    real_move = _shutil.move

    def tracking_move(src: str, dst: str) -> str:
        moved.append((src, dst))
        return real_move(src, dst)

    with patch("sandbox.layer_stack.stack.os.replace", side_effect=replace_raises_exdev), \
            patch("sandbox.layer_stack.stack.shutil.move", side_effect=tracking_move):
        _replace_workspace_contents(destination, source)

    # Every source child should have been moved via shutil.move
    assert len(moved) == 2

    assert (destination / "alpha.txt").read_text(encoding="utf-8") == "alpha"
    assert (destination / "beta" / "inner.txt").read_text(encoding="utf-8") == "beta-inner"

    # real os.replace still works for sanity
    assert real_replace is os.replace


def test_replace_workspace_contents_propagates_non_exdev_oserror(
    tmp_path: Path,
) -> None:
    destination = tmp_path / "dst"
    source = tmp_path / "src"
    destination.mkdir()
    source.mkdir()
    (source / "alpha.txt").write_text("alpha", encoding="utf-8")

    def replace_raises_eacces(src: object, dst: object) -> None:
        raise OSError(errno.EACCES, "Permission denied")

    with patch("sandbox.layer_stack.stack.os.replace", side_effect=replace_raises_eacces):
        with pytest.raises(OSError) as excinfo:
            _replace_workspace_contents(destination, source)
        assert excinfo.value.errno == errno.EACCES
