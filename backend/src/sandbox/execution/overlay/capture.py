"""Capture raw filesystem changes from a snapshot overlay upperdir."""

from __future__ import annotations

import os
import shutil
import stat
from collections.abc import Iterator
from contextlib import suppress
from pathlib import Path

from sandbox.layer_stack.layer.index import OPAQUE_MARKER, WHITEOUT_PREFIX
from sandbox.layer_stack.workspace.base import _relative_target_escapes
from sandbox.execution.overlay.change import OverlayPathChange, content_hash
from sandbox.timing import monotonic_now


def capture_changes(
    upperdir: str | Path,
    *,
    lowerdir: str | Path | None = None,
    workspace_root: str | Path | None = None,
    timings: dict[str, float] | None = None,
) -> tuple[OverlayPathChange, ...]:
    """Return raw upperdir changes for one leased snapshot shell call.

    The production path reads the actual overlay upperdir. Unit and local
    runtimes can pass ``lowerdir`` and ``workspace_root`` to populate that
    upperdir from a copy-backed merged view before capture.
    """
    upper_root = Path(upperdir)
    upper_root.mkdir(parents=True, exist_ok=True)
    if lowerdir is not None and workspace_root is not None:
        populate_start = monotonic_now()
        _populate_upperdir_from_diff(
            lowerdir=Path(lowerdir),
            workspace_root=Path(workspace_root),
            upperdir=upper_root,
        )
        if timings is not None:
            timings["overlay.capture.populate_upperdir_s"] = (
                monotonic_now() - populate_start
            )
    walk_start = monotonic_now()
    changes = tuple(_walk_upperdir(upper_root))
    if timings is not None:
        timings["overlay.capture.walk_upperdir_s"] = monotonic_now() - walk_start
    return changes


# Copy-backed population.


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
        if _has_nondirectory_payload_ancestor(
            rel,
            merged_paths,
            root=workspace_root,
        ):
            continue
        _write_whiteout(upperdir, rel)

    for rel in sorted(merged_paths):
        merged_entry = workspace_root / rel
        lower_entry = lowerdir / rel
        if rel in lower_paths and _entries_match(lower_entry, merged_entry):
            continue
        target = upperdir / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        _remove_path(target)
        if merged_entry.is_symlink():
            link_target = os.readlink(merged_entry)
            if link_target.startswith("/") or _relative_target_escapes(link_target):
                raise ValueError(
                    "overlay capture refuses escaping symlink target: "
                    f"{rel.as_posix()}"
                )
            os.symlink(link_target, target)
        elif merged_entry.is_dir():
            target.mkdir(parents=True, exist_ok=True)
            with suppress(OSError):
                shutil.copystat(merged_entry, target, follow_symlinks=False)
            if not _has_payload_descendant(rel, merged_paths):
                (target / OPAQUE_MARKER).write_text("", encoding="utf-8")
        elif merged_entry.is_file():
            shutil.copy2(merged_entry, target)


def _payload_paths(root: Path) -> set[Path]:
    if not root.exists():
        return set()
    paths: set[Path] = set()
    for entry in root.rglob("*"):
        if entry.name == OPAQUE_MARKER or entry.name.startswith(WHITEOUT_PREFIX):
            continue
        if entry.is_symlink() or entry.is_file() or entry.is_dir():
            paths.add(entry.relative_to(root))
    return paths


def _entries_match(left: Path, right: Path) -> bool:
    if left.is_symlink() or right.is_symlink():
        return (
            left.is_symlink()
            and right.is_symlink()
            and os.readlink(left) == os.readlink(right)
        )
    if left.is_dir() and right.is_dir():
        return _mode_bits(left) == _mode_bits(right)
    if left.is_file() and right.is_file():
        return (
            left.read_bytes() == right.read_bytes()
            and _mode_bits(left) == _mode_bits(right)
        )
    return False


def _has_nondirectory_payload_ancestor(
    rel: Path,
    payload_paths: set[Path],
    *,
    root: Path,
) -> bool:
    parts = rel.parts
    for index in range(1, len(parts)):
        ancestor = Path(*parts[:index])
        if ancestor not in payload_paths:
            continue
        entry = root / ancestor
        if entry.is_symlink() or entry.is_file():
            return True
    return False


def _has_payload_descendant(rel: Path, payload_paths: set[Path]) -> bool:
    prefix = rel.parts
    return any(
        len(path.parts) > len(prefix) and path.parts[: len(prefix)] == prefix
        for path in payload_paths
    )


def _mode_bits(path: Path) -> int:
    return stat.S_IMODE(path.lstat().st_mode)


# Upperdir walking.


def _walk_upperdir(upper_root: Path) -> Iterator[OverlayPathChange]:
    emitted_opaque_dirs: set[str] = set()
    for entry in sorted(upper_root.rglob("*"), key=lambda item: item.as_posix()):
        rel = entry.relative_to(upper_root)
        if entry.name == OPAQUE_MARKER:
            opaque_path = rel.parent.as_posix() if rel.parent.as_posix() != "." else ""
            if opaque_path not in emitted_opaque_dirs:
                emitted_opaque_dirs.add(opaque_path)
                yield OverlayPathChange(
                    path=opaque_path,
                    kind="opaque_dir",
                    content_path=None,
                    final_hash=None,
                )
            continue
        if _is_whiteout_marker(entry):
            yield OverlayPathChange(
                path=_whiteout_target(rel).as_posix(),
                kind="delete",
                content_path=None,
                final_hash=None,
            )
            continue
        if entry.is_dir():
            if _has_overlay_opaque_xattr(entry):
                opaque_path = rel.as_posix()
                if opaque_path not in emitted_opaque_dirs:
                    emitted_opaque_dirs.add(opaque_path)
                    yield OverlayPathChange(
                        path=opaque_path,
                        kind="opaque_dir",
                        content_path=None,
                        final_hash=None,
                    )
            continue
        if _is_overlay_whiteout(entry):
            yield OverlayPathChange(
                path=rel.as_posix(),
                kind="delete",
                content_path=None,
                final_hash=None,
            )
            continue
        if entry.is_symlink():
            yield OverlayPathChange(
                path=rel.as_posix(),
                kind="symlink",
                content_path=str(entry),
                final_hash=content_hash(entry, symlink=True),
            )
            continue
        if entry.is_file():
            yield OverlayPathChange(
                path=rel.as_posix(),
                kind="write",
                content_path=str(entry),
                final_hash=content_hash(entry),
            )


def _write_whiteout(upperdir: Path, rel: Path) -> None:
    marker = upperdir / rel.parent / f"{WHITEOUT_PREFIX}{rel.name}"
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text("", encoding="utf-8")


def _remove_path(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink(missing_ok=True)
    elif path.is_dir():
        shutil.rmtree(path)


# Overlay marker decoding.


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
