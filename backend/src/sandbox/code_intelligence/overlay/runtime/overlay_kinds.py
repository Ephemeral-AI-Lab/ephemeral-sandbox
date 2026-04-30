"""Overlayfs upperdir kind detection."""

from __future__ import annotations

import os
import stat


def is_whiteout(st: os.stat_result, xattrs: dict[bytes, bytes]) -> bool:
    """True when *st* is an overlay whiteout."""
    if stat.S_ISCHR(st.st_mode) and st.st_rdev == 0:
        return True
    if stat.S_ISREG(st.st_mode) and st.st_size == 0:
        return b"user.overlay.whiteout" in xattrs
    return False


def is_opaque_dir(st: os.stat_result, xattrs: dict[bytes, bytes]) -> bool:
    """True when *st* marks an overlay opaque directory."""
    if not stat.S_ISDIR(st.st_mode):
        return False
    return (
        xattrs.get(b"trusted.overlay.opaque") == b"y"
        or xattrs.get(b"user.overlay.opaque") == b"y"
    )


def is_symlink(st: os.stat_result) -> bool:
    return stat.S_ISLNK(st.st_mode)


__all__ = ["is_opaque_dir", "is_symlink", "is_whiteout"]
