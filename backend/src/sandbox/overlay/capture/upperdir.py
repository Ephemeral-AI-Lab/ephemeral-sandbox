"""Capture raw filesystem changes from an overlay upperdir."""

from __future__ import annotations

import os
import shutil
import stat
from collections.abc import Iterator
from pathlib import Path

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.merged_view import OPAQUE_MARKER, WHITEOUT_PREFIX
from sandbox.overlay.capture.changes import UpperChange, content_hash


def capture_changes(
    upperdir: str | Path,
    *,
    snapshot_manifest: Manifest,
    lowerdir: str | Path | None = None,
    workspace_root: str | Path | None = None,
) -> tuple[UpperChange, ...]:
    """Return raw upperdir changes for one leased snapshot shell call.

    The production path reads the actual overlay upperdir. Unit and local
    runtimes can pass ``lowerdir`` and ``workspace_root`` to populate that
    upperdir from a copy-backed merged view before capture.
    """
    del snapshot_manifest
    upper_root = Path(upperdir)
    upper_root.mkdir(parents=True, exist_ok=True)
    if lowerdir is not None and workspace_root is not None:
        _populate_upperdir_from_diff(
            lowerdir=Path(lowerdir),
            workspace_root=Path(workspace_root),
            upperdir=upper_root,
        )
    return tuple(_walk_upperdir(upper_root))


def _populate_upperdir_from_diff(
    *,
    lowerdir: Path,
    workspace_root: Path,
    upperdir: Path,
) -> None:
    if upperdir.exists():
        shutil.rmtree(upperdir)
    upperdir.mkdir(parents=True)

    lower_paths = _payload_paths(lowerdir)
    merged_paths = _payload_paths(workspace_root)

    for rel in sorted(lower_paths - merged_paths):
        _write_whiteout(upperdir, rel)

    for rel in sorted(merged_paths):
        merged_entry = workspace_root / rel
        lower_entry = lowerdir / rel
        if rel in lower_paths and _entries_match(lower_entry, merged_entry):
            continue
        target = upperdir / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        if merged_entry.is_symlink():
            os.symlink(os.readlink(merged_entry), target)
        elif merged_entry.is_file():
            shutil.copy2(merged_entry, target)


def _payload_paths(root: Path) -> set[Path]:
    if not root.exists():
        return set()
    paths: set[Path] = set()
    for entry in root.rglob("*"):
        if entry.name == OPAQUE_MARKER or entry.name.startswith(WHITEOUT_PREFIX):
            continue
        if entry.is_symlink() or entry.is_file():
            paths.add(entry.relative_to(root))
    return paths


def _entries_match(left: Path, right: Path) -> bool:
    if left.is_symlink() or right.is_symlink():
        return left.is_symlink() and right.is_symlink() and os.readlink(left) == os.readlink(right)
    if left.is_file() and right.is_file():
        return left.read_bytes() == right.read_bytes()
    return False


def _walk_upperdir(upper_root: Path) -> Iterator[UpperChange]:
    for entry in sorted(upper_root.rglob("*"), key=lambda item: item.as_posix()):
        rel = entry.relative_to(upper_root)
        if entry.name == OPAQUE_MARKER:
            yield UpperChange(
                path=rel.parent.as_posix() if rel.parent.as_posix() != "." else "",
                kind="opaque_dir",
                content_path=None,
                final_hash=None,
            )
            continue
        if _is_whiteout_marker(entry):
            yield UpperChange(
                path=_whiteout_target(rel).as_posix(),
                kind="delete",
                content_path=None,
                final_hash=None,
            )
            continue
        if entry.is_dir():
            if _has_overlay_opaque_xattr(entry):
                yield UpperChange(
                    path=rel.as_posix(),
                    kind="opaque_dir",
                    content_path=None,
                    final_hash=None,
                )
            continue
        if _is_overlay_whiteout(entry):
            yield UpperChange(
                path=rel.as_posix(),
                kind="delete",
                content_path=None,
                final_hash=None,
            )
            continue
        if entry.is_symlink():
            yield UpperChange(
                path=rel.as_posix(),
                kind="symlink",
                content_path=str(entry),
                final_hash=content_hash(entry, symlink=True),
            )
            continue
        if entry.is_file():
            yield UpperChange(
                path=rel.as_posix(),
                kind="write",
                content_path=str(entry),
                final_hash=content_hash(entry),
            )


def _write_whiteout(upperdir: Path, rel: Path) -> None:
    marker = upperdir / rel.parent / f"{WHITEOUT_PREFIX}{rel.name}"
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text("", encoding="utf-8")


def _is_whiteout_marker(entry: Path) -> bool:
    return entry.name.startswith(WHITEOUT_PREFIX) and entry.name != OPAQUE_MARKER


def _whiteout_target(rel: Path) -> Path:
    return rel.parent / rel.name[len(WHITEOUT_PREFIX) :]


def _is_overlay_whiteout(entry: Path) -> bool:
    try:
        st = entry.lstat()
    except FileNotFoundError:
        return False
    if stat.S_ISCHR(st.st_mode) and getattr(st, "st_rdev", None) in (0, None):
        return True
    return entry.is_file() and entry.stat().st_size == 0 and _has_xattr(
        entry,
        b"user.overlay.whiteout",
    )


def _has_overlay_opaque_xattr(entry: Path) -> bool:
    return _xattr_value(entry, b"trusted.overlay.opaque") == b"y" or _xattr_value(
        entry,
        b"user.overlay.opaque",
    ) == b"y"


def _has_xattr(path: Path, key: bytes) -> bool:
    return _xattr_value(path, key) is not None


def _xattr_value(path: Path, key: bytes) -> bytes | None:
    getxattr = getattr(os, "getxattr", None)
    if getxattr is None:
        return None
    try:
        return getxattr(path, key, follow_symlinks=False)
    except OSError:
        return None


__all__ = ["capture_changes"]
