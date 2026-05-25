"""Walk an overlay upperdir into ``OverlayPathChange`` tuples."""

from __future__ import annotations

import os
import stat
from collections.abc import Iterator
from pathlib import Path

from sandbox.layer_stack.layer_index import OPAQUE_MARKER, WHITEOUT_PREFIX
from sandbox.overlay.path_change import (
    OverlayPathChange,
    OverlayPathChangeKind,
    content_hash,
)
from sandbox._shared.clock import monotonic_now


def walk_upperdir(
    upper_root: str | Path,
    *,
    timings: dict[str, float] | None = None,
) -> tuple[OverlayPathChange, ...]:
    """Return raw upperdir changes for one leased snapshot shell call."""
    root = Path(upper_root)
    root.mkdir(parents=True, exist_ok=True)
    walk_start = monotonic_now()
    changes = tuple(_walk_upperdir(root))
    if timings is not None:
        timings["overlay.capture.walk_upperdir_s"] = monotonic_now() - walk_start
    return changes


def _marker(kind: OverlayPathChangeKind, path: str) -> OverlayPathChange:
    return OverlayPathChange(path=path, kind=kind, content_path=None, final_hash=None)


def _content(
    kind: OverlayPathChangeKind, path: str, entry: Path, *, symlink: bool = False
) -> OverlayPathChange:
    return OverlayPathChange(
        path=path,
        kind=kind,
        content_path=str(entry),
        final_hash=content_hash(entry, symlink=symlink),
    )


def _walk_upperdir(upper_root: Path) -> Iterator[OverlayPathChange]:
    # Stream via os.walk with per-level sort instead of sorted(rglob("*")),
    # which materializes every path in memory before iterating — OOM-prone
    # on runaway commands that write many files. Emission order changes from
    # full-tree lex to per-level lex; consumers depend on the
    # "opaque_dir before children" invariant which topdown=True preserves.
    emitted_opaque_dirs: set[str] = set()
    for dirpath, dirnames, filenames in os.walk(upper_root, topdown=True, followlinks=False):
        dirnames.sort()
        filenames.sort()
        dir_path = Path(dirpath)
        for name in filenames:
            entry = dir_path / name
            rel = entry.relative_to(upper_root)
            if name == OPAQUE_MARKER:
                opaque_path = rel.parent.as_posix() if rel.parent.as_posix() != "." else ""
                if opaque_path not in emitted_opaque_dirs:
                    emitted_opaque_dirs.add(opaque_path)
                    yield _marker("opaque_dir", opaque_path)
                continue
            if _is_whiteout_marker(entry):
                yield _marker("delete", _whiteout_target(rel).as_posix())
                continue
            if _is_overlay_whiteout(entry):
                yield _marker("delete", rel.as_posix())
                continue
            if entry.is_symlink():
                yield _content("symlink", rel.as_posix(), entry, symlink=True)
                continue
            if entry.is_file():
                yield _content("write", rel.as_posix(), entry)
        for name in dirnames:
            entry = dir_path / name
            if not _has_overlay_opaque_xattr(entry):
                continue
            rel = entry.relative_to(upper_root)
            opaque_path = rel.as_posix()
            if opaque_path not in emitted_opaque_dirs:
                emitted_opaque_dirs.add(opaque_path)
                yield _marker("opaque_dir", opaque_path)


def _is_whiteout_marker(entry: Path) -> bool:
    # A literal ``.wh.`` entry has no target name and would fail later during
    # layer path normalization, so require a non-empty suffix.
    return (
        entry.name.startswith(WHITEOUT_PREFIX)
        and entry.name != OPAQUE_MARKER
        and len(entry.name) > len(WHITEOUT_PREFIX)
    )


def _whiteout_target(rel: Path) -> Path:
    return rel.parent / rel.name[len(WHITEOUT_PREFIX) :]


def _is_overlay_whiteout(entry: Path) -> bool:
    try:
        st = entry.lstat()
    except FileNotFoundError:
        return False
    # The overlayfs whiteout convention is ``S_ISCHR(mode) && rdev ==
    # makedev(0, 0)``. Do not treat missing ``st_rdev`` as a whiteout.
    if stat.S_ISCHR(st.st_mode) and getattr(st, "st_rdev", None) == 0:
        return True
    return (
        entry.is_file()
        and entry.stat().st_size == 0
        and _xattr_value(entry, b"user.overlay.whiteout") is not None
    )


def _has_overlay_opaque_xattr(entry: Path) -> bool:
    return (
        _xattr_value(entry, b"trusted.overlay.opaque") == b"y"
        or _xattr_value(
            entry,
            b"user.overlay.opaque",
        )
        == b"y"
    )


def _xattr_value(path: Path, key: bytes) -> bytes | None:
    getxattr = getattr(os, "getxattr", None)
    if getxattr is None:
        return None
    try:
        return getxattr(path, key, follow_symlinks=False)
    except OSError:
        return None


__all__ = ["walk_upperdir"]
