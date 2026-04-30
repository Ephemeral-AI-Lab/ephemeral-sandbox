"""Upperdir walking and xattr reads for the overlay runtime."""

from __future__ import annotations

import os
from collections.abc import Iterator

from .overlay_kinds import is_opaque_dir
from .types import UpperEntry


def walk_upperdir(upper_root: str) -> Iterator[UpperEntry]:
    """Yield one :class:`UpperEntry` per non-directory upperdir entry."""
    upper_root = upper_root.rstrip("/")
    if not os.path.isdir(upper_root):
        return
    for dirpath, dirnames, filenames in os.walk(
        upper_root, topdown=True, followlinks=False
    ):
        rel_dir = os.path.relpath(dirpath, upper_root)
        rel_dir = "" if rel_dir == "." else rel_dir

        if rel_dir:
            full = os.path.join(upper_root, rel_dir)
            try:
                st = os.lstat(full)
            except FileNotFoundError:
                pass
            else:
                xattrs = _read_xattrs(full)
                if is_opaque_dir(st, xattrs):
                    yield UpperEntry(
                        rel=rel_dir, st=st, xattrs=xattrs, upper_path=full
                    )

        for name in filenames:
            rel = os.path.join(rel_dir, name) if rel_dir else name
            full = os.path.join(dirpath, name)
            try:
                st = os.lstat(full)
            except FileNotFoundError:
                continue
            xattrs = _read_xattrs(full)
            yield UpperEntry(rel=rel, st=st, xattrs=xattrs, upper_path=full)

        dirnames.sort()


def _read_xattrs(path: str) -> dict[bytes, bytes]:
    listxattr = getattr(os, "listxattr", None)
    getxattr = getattr(os, "getxattr", None)
    if listxattr is None or getxattr is None:
        return {}
    try:
        names = listxattr(path, follow_symlinks=False)
    except OSError:
        return {}
    out: dict[bytes, bytes] = {}
    for name in names:
        key = name.encode("utf-8") if isinstance(name, str) else name
        try:
            out[key] = getxattr(path, name, follow_symlinks=False)
        except OSError:
            continue
    return out


__all__ = ["walk_upperdir"]
