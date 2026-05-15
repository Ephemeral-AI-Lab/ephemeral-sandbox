"""Capture raw filesystem changes from a snapshot overlay upperdir."""

from __future__ import annotations

import os
import shutil
import stat
from collections.abc import Iterator
from contextlib import suppress
from pathlib import Path

from sandbox.layer_stack.paths import relative_symlink_target_escapes
from sandbox.layer_stack.layer_index import OPAQUE_MARKER, WHITEOUT_PREFIX
from sandbox.execution.overlay_change import (
    OverlayPathChange,
    OverlayPathChangeKind,
    content_hash,
)
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
    if timings is None:
        timings = {}
    if lowerdir is not None and workspace_root is not None:
        populate_start = monotonic_now()
        _populate_upperdir_from_diff(
            lowerdir=Path(lowerdir),
            workspace_root=Path(workspace_root),
            upperdir=upper_root,
        )
        timings["overlay.capture.populate_upperdir_s"] = (
            monotonic_now() - populate_start
        )
    walk_start = monotonic_now()
    changes = tuple(_walk_upperdir(upper_root))
    timings["overlay.capture.walk_upperdir_s"] = monotonic_now() - walk_start
    return changes


# Copy-backed population.


def _populate_upperdir_from_diff(
    *,
    lowerdir: Path,
    workspace_root: Path,
    upperdir: Path,
) -> None:
    # Caller (`capture_changes`) has already mkdir'd `upperdir` exist_ok; we
    # reset to a clean slate before materializing the diff.
    shutil.rmtree(upperdir, ignore_errors=True)
    upperdir.mkdir(parents=True)

    lower_paths = _payload_paths(lowerdir)
    merged_paths = _payload_paths(workspace_root)
    # Prefix index: every dir that has at least one descendant in merged_paths.
    # O(N) to build, O(1) per lookup — replaces the O(N²) inner scan that
    # `_has_payload_descendant` used to do.
    dirs_with_descendants: set[Path] = {
        parent for path in merged_paths for parent in path.parents if parent != Path(".")
    }

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
            if link_target.startswith("/") or relative_symlink_target_escapes(link_target):
                raise ValueError(
                    "overlay capture refuses escaping symlink target: "
                    f"{rel.as_posix()}"
                )
            os.symlink(link_target, target)
        elif merged_entry.is_dir():
            target.mkdir(parents=True, exist_ok=True)
            with suppress(OSError):
                shutil.copystat(merged_entry, target, follow_symlinks=False)
            if rel not in dirs_with_descendants:
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


def _mode_bits(path: Path) -> int:
    return stat.S_IMODE(path.lstat().st_mode)


# Upperdir walking.


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
    for dirpath, dirnames, filenames in os.walk(
        upper_root, topdown=True, followlinks=False
    ):
        dirnames.sort()
        filenames.sort()
        dir_path = Path(dirpath)
        for name in filenames:
            entry = dir_path / name
            rel = entry.relative_to(upper_root)
            if name == OPAQUE_MARKER:
                opaque_path = (
                    rel.parent.as_posix() if rel.parent.as_posix() != "." else ""
                )
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
