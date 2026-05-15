"""Private filesystem/path helpers for layer-stack storage components."""

from __future__ import annotations

import os
import shutil
from collections.abc import Callable
from pathlib import Path, PurePosixPath


def join_layer_path(root: Path, rel: str) -> Path:
    if not rel:
        return root
    return root.joinpath(*PurePosixPath(rel).parts)


def remove_path(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink(missing_ok=True)
    elif path.is_dir():
        shutil.rmtree(path)


def resolve_storage_path(storage_root: Path, path: str) -> Path:
    if "\0" in path:
        raise ValueError(f"layer path must not contain NUL bytes: {path!r}")
    candidate = Path(path)
    if candidate.is_absolute():
        raise ValueError(f"layer path must be relative: {path}")
    joined = storage_root / candidate
    resolved = joined.resolve(strict=False)
    storage_resolved = storage_root.resolve(strict=False)
    if resolved != storage_resolved and not resolved.is_relative_to(
        storage_resolved
    ):
        raise ValueError(
            f"layer path escapes storage_root: {path!r} -> {resolved}"
        )
    return joined


def fsync_path(path: Path) -> None:
    """fsync a regular file or directory by path."""
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def relative_symlink_target_escapes(target: str) -> bool:
    """Return True if a relative symlink target walks out of its directory."""
    depth = 0
    for part in target.split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            if depth == 0:
                return True
            depth -= 1
        else:
            depth += 1
    return False


def allocate_unique_layer_paths(
    *,
    storage_root: Path,
    layers_dir: str,
    staging_dir: str,
    next_version: int,
    id_factory: Callable[[int], str],
    attempts: int = 100,
) -> tuple[str, Path, Path]:
    for _ in range(attempts):
        layer_id = id_factory(next_version)
        layer_dir = storage_root / layers_dir / layer_id
        pending_staging_dir = storage_root / staging_dir / f"{layer_id}.staging"
        if not layer_dir.exists() and not pending_staging_dir.exists():
            return layer_id, pending_staging_dir, layer_dir
    raise RuntimeError("could not allocate a unique layer id")


__all__ = [
    "allocate_unique_layer_paths",
    "fsync_path",
    "join_layer_path",
    "relative_symlink_target_escapes",
    "remove_path",
    "resolve_storage_path",
]
