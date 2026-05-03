"""Unit tests for the capture-only overlay runtime helpers."""

from __future__ import annotations

import os
import stat
from pathlib import Path

from sandbox.overlay.runtime.capture import (
    is_opaque_dir,
    is_whiteout,
    walk_upperdir,
)
from sandbox.overlay.runtime.cli import (
    REJECT_UPPER_FULL,
    reject_exit_code,
)
from sandbox.overlay.runtime.types import UpperEntry


def _fake_stat(*, mode: int, size: int = 0, rdev: int = 0) -> os.stat_result:
    return os.stat_result((mode, 0, 0, 0, 0, 0, size, 0, 0, 0, rdev))


def test_is_whiteout_privileged_char_device() -> None:
    st = _fake_stat(mode=stat.S_IFCHR | 0o000, rdev=0)
    assert is_whiteout(st, {}) is True


def test_is_whiteout_rootless_userxattr_zero_size_regular() -> None:
    st = _fake_stat(mode=stat.S_IFREG | 0o600, size=0)
    assert is_whiteout(st, {b"user.overlay.whiteout": b""}) is True


def test_is_whiteout_false_when_regular_non_zero_without_xattr() -> None:
    st = _fake_stat(mode=stat.S_IFREG | 0o600, size=1)
    assert is_whiteout(st, {}) is False


def test_is_opaque_dir_both_xattr_namespaces() -> None:
    st = _fake_stat(mode=stat.S_IFDIR | 0o700)
    assert is_opaque_dir(st, {b"trusted.overlay.opaque": b"y"}) is True
    assert is_opaque_dir(st, {b"user.overlay.opaque": b"y"}) is True
    assert is_opaque_dir(st, {}) is False


def test_walk_upperdir_yields_regular_files(tmp_path: Path) -> None:
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "app.py").write_text("hi\n", encoding="utf-8")

    entries = list(walk_upperdir(str(tmp_path)))

    assert [entry.rel for entry in entries] == ["pkg/app.py"]
    assert isinstance(entries[0], UpperEntry)


def test_reject_exit_code_for_upper_full_is_distinct() -> None:
    assert reject_exit_code(REJECT_UPPER_FULL) == 207
    assert reject_exit_code("unknown") == 200
