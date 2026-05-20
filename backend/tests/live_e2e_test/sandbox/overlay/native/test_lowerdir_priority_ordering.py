"""Pin the lowerdir+= priority ordering contract for the new mount API.

Kernel guarantee (overlayfs.rst, "Multiple lower layers"):
    "The specified lower directories will be stacked beginning from the
    rightmost one and going left. In the above example lower1 will be the
    top, lower2 the middle and lower3 the bottom layer."

For fsconfig(FSCONFIG_SET_STRING, "lowerdir+", path) calls:
    FIRST call = leftmost in the conceptual lowerdir list = TOP priority.

This means: if manifest.layers is ordered newest-first, iterate it in
order (no reversal) when calling lowerdir+ — the first call (newest layer)
wins file conflicts.

This test MUST FAIL if the kernel changes this ordering.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import shutil
import sys
import tempfile
from pathlib import Path

import pytest


# Skip the entire module on non-Linux: the fsopen/fsconfig/fsmount syscalls
# don't exist on Darwin or Windows.
pytestmark = pytest.mark.skipif(
    sys.platform != "linux",
    reason="lowerdir+ ordering test requires Linux (fsopen/fsconfig syscalls)",
)


# ---------------------------------------------------------------------------
# Minimal ctypes bindings — same pattern as overlay_bisect.py
# ---------------------------------------------------------------------------

_libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
_libc.syscall.restype = ctypes.c_long

_SYS_fsopen     = 430
_SYS_fsconfig   = 431
_SYS_fsmount    = 432
_SYS_move_mount = 429

_FSCONFIG_SET_STRING = 1
_FSCONFIG_CMD_CREATE = 6
_MOVE_MOUNT_F_EMPTY_PATH = 4
_AT_FDCWD = -100


def _fsopen(fsname: bytes) -> int:
    rc = _libc.syscall(_SYS_fsopen, fsname, 0)
    if rc < 0:
        raise OSError(ctypes.get_errno(), "fsopen")
    return rc


def _fsconfig_string(fd: int, key: bytes, value: bytes) -> None:
    rc = _libc.syscall(_SYS_fsconfig, fd, _FSCONFIG_SET_STRING, key, value, 0)
    if rc < 0:
        raise OSError(ctypes.get_errno(), f"fsconfig({key!r}={value!r})")


def _fsconfig_create(fd: int) -> None:
    rc = _libc.syscall(_SYS_fsconfig, fd, _FSCONFIG_CMD_CREATE, 0, 0, 0)
    if rc < 0:
        raise OSError(ctypes.get_errno(), "fsconfig CREATE")


def _fsmount(fd: int) -> int:
    rc = _libc.syscall(_SYS_fsmount, fd, 0, 0)
    if rc < 0:
        raise OSError(ctypes.get_errno(), "fsmount")
    return rc


def _move_mount(from_fd: int, target: bytes) -> None:
    rc = _libc.syscall(
        _SYS_move_mount, from_fd, b"", _AT_FDCWD, target,
        _MOVE_MOUNT_F_EMPTY_PATH,
    )
    if rc < 0:
        raise OSError(ctypes.get_errno(), "move_mount")


# ---------------------------------------------------------------------------
# Helper: mount read-only overlay via lowerdir+ and return merged marker
# ---------------------------------------------------------------------------

def _mount_and_read_marker(layer_dirs: list[Path], merged: Path) -> str:
    """Call lowerdir+ in the order given by layer_dirs, return marker.txt content."""
    fd = _fsopen(b"overlay")
    try:
        for d in layer_dirs:
            _fsconfig_string(fd, b"lowerdir+", str(d).encode())
        _fsconfig_create(fd)
        mfd = _fsmount(fd)
        try:
            _move_mount(mfd, str(merged).encode())
        finally:
            os.close(mfd)
    finally:
        os.close(fd)
    return (merged / "marker.txt").read_text().strip()


# ---------------------------------------------------------------------------
# The canonical ordering assertion
# ---------------------------------------------------------------------------

def test_lowerdir_plus_first_call_is_top_priority(tmp_path: Path) -> None:
    """FIRST lowerdir+ call = highest priority (top of stack).

    Layers A, B, C are added in that order. All three have marker.txt with
    their letter. The kernel must serve 'A' — proving first-call = top.

    This test FAILS if the kernel reverses the ordering.
    """
    # Build three lower dirs each containing marker.txt = letter
    layer_dirs = []
    for letter in ("A", "B", "C"):
        d = tmp_path / letter
        d.mkdir()
        (d / "marker.txt").write_text(letter)
        layer_dirs.append(d)

    merged = tmp_path / "merged"
    merged.mkdir()

    try:
        result = _mount_and_read_marker(layer_dirs, merged)
    except OSError as exc:
        pytest.skip(f"Overlay mount failed (missing CAP_SYS_ADMIN?): {exc}")
    finally:
        os.system(f"umount {merged} 2>/dev/null")

    # Hard assertion: must be "A" — if this fails, kernel changed ordering.
    assert result == "A", (
        f"lowerdir+ ordering contract violated: expected first-call layer 'A' "
        f"to win, got '{result}'. "
        f"The new-mount-API lowerdir+ iteration order in kernel_mount.py must "
        f"be updated to match the new kernel semantics."
    )


def test_lowerdir_plus_last_call_is_bottom_priority(tmp_path: Path) -> None:
    """LAST lowerdir+ call = lowest priority (bottom of stack).

    Layers A, B, C are added in that order. All three have marker.txt.
    The kernel must NOT serve 'C' (last call). This redundantly pins the
    bottom boundary as a second guard.
    """
    layer_dirs = []
    for letter in ("A", "B", "C"):
        d = tmp_path / f"bottom_{letter}"
        d.mkdir()
        (d / "marker.txt").write_text(letter)
        layer_dirs.append(d)

    merged = tmp_path / "merged_bottom"
    merged.mkdir()

    try:
        result = _mount_and_read_marker(layer_dirs, merged)
    except OSError as exc:
        pytest.skip(f"Overlay mount failed (missing CAP_SYS_ADMIN?): {exc}")
    finally:
        os.system(f"umount {merged} 2>/dev/null")

    assert result != "C", (
        f"lowerdir+ ordering contract violated: last-call layer 'C' should be "
        f"bottom priority but it won. Got '{result}'."
    )
