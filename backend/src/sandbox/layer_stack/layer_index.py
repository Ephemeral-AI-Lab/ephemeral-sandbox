"""Per-layer presence index for fast `MergedView.read_bytes` negative lookups.

A layer directory is immutable once published, so the index is built lazily
once per process and cached by ``layer_id``.

Each index records:

- ``files`` — paths that resolve to a regular file or symlink at that exact
  location in the layer.
- ``whiteouts`` — paths covered by a ``.wh.{name}`` sibling marker; consulting
  the index lets the merged view return ``(None, False)`` without a stat.
- ``opaque_dirs`` — directory paths carrying ``.wh..wh..opq``; younger layers
  are blocked from showing through these directories.

The merged view's read path uses these sets to short-circuit the per-layer
filesystem walk for paths that are provably absent — the dominant cost for
shell captures that create many new files.
"""

from __future__ import annotations

import os
import stat
from dataclasses import dataclass
from pathlib import Path, PurePosixPath


WHITEOUT_PREFIX = ".wh."
OPAQUE_MARKER = ".wh..wh..opq"


@dataclass(frozen=True)
class LayerIndex:
    files: frozenset[str]
    whiteouts: frozenset[str]
    opaque_dirs: frozenset[str]


def build_layer_index(layer_dir: Path) -> LayerIndex:
    files: set[str] = set()
    whiteouts: set[str] = set()
    opaque_dirs: set[str] = set()

    for entry in layer_dir.rglob("*"):
        rel = PurePosixPath(entry.relative_to(layer_dir).as_posix())
        if entry.name == OPAQUE_MARKER:
            parent = rel.parent.as_posix()
            opaque_dirs.add("" if parent == "." else parent)
            continue
        if entry.name.startswith(WHITEOUT_PREFIX):
            target_name = entry.name[len(WHITEOUT_PREFIX) :]
            parent = rel.parent.as_posix()
            target = target_name if parent == "." else f"{parent}/{target_name}"
            whiteouts.add(target)
            continue
        if _is_kernel_whiteout(entry):
            whiteouts.add(rel.as_posix())
            continue
        if entry.is_symlink() or entry.is_file():
            files.add(rel.as_posix())

    return LayerIndex(
        files=frozenset(files),
        whiteouts=frozenset(whiteouts),
        opaque_dirs=frozenset(opaque_dirs),
    )


def _is_kernel_whiteout(entry: Path) -> bool:
    try:
        st = entry.lstat()
    except FileNotFoundError:
        return False
    if stat.S_ISCHR(st.st_mode) and getattr(st, "st_rdev", None) == 0:
        return True
    if not stat.S_ISREG(st.st_mode) or st.st_size != 0:
        return False
    return _xattr_value(entry, b"trusted.overlay.whiteout") is not None or _xattr_value(
        entry, b"user.overlay.whiteout"
    ) is not None


def _xattr_value(path: Path, key: bytes) -> bytes | None:
    getxattr = getattr(os, "getxattr", None)
    if getxattr is None:
        return None
    try:
        return getxattr(path, key, follow_symlinks=False)
    except OSError:
        return None


def has_ancestor_in(rel: str, members: frozenset[str]) -> bool:
    """Return True if any strict ancestor of ``rel`` is in ``members``."""
    if not members:
        return False
    parts = PurePosixPath(rel).parts
    return any("/".join(parts[:index]) in members for index in range(1, len(parts)))


__all__ = [
    "LayerIndex",
    "OPAQUE_MARKER",
    "WHITEOUT_PREFIX",
    "build_layer_index",
    "has_ancestor_in",
]
