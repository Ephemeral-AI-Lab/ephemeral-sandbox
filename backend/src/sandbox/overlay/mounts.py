"""Copy-backed workspace preparation for one snapshot overlay request.

This module intentionally does not enter a Linux mount namespace. The
privileged kernel-overlay entrypoint is
``sandbox.command_exec.workspace.namespace_entrypoint``; this portable path
materializes the leased snapshot into a normal directory tree and captures the
diff afterward.
"""

from __future__ import annotations

import shutil
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.view.merged import MergedView
from sandbox.timing import monotonic_now

_INTERMEDIATE_RUN_DIRS: tuple[str, ...] = ("lower", "merged", "work")


@dataclass(frozen=True)
class OverlayMountedSnapshot:
    manifest: Manifest
    workspace_root: str
    lowerdir: str
    upperdir: str
    workdir: str


def mount_snapshot(
    *,
    manifest: Manifest,
    storage_root: str | Path,
    run_dir: str | Path,
    timings: dict[str, float] | None = None,
) -> OverlayMountedSnapshot:
    """Create a runtime-local merged workspace for a leased manifest."""
    run_root = Path(run_dir)
    lowerdir = run_root / "lower"
    upperdir = run_root / "upper"
    workdir = run_root / "work"
    merged = run_root / "merged"

    prepare_start = monotonic_now()
    for directory in (upperdir, workdir, merged):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    if timings is not None:
        timings["overlay.mount.prepare_dirs_s"] = monotonic_now() - prepare_start

    materialize_start = monotonic_now()
    MergedView(storage_root).materialize(lowerdir, manifest)
    if timings is not None:
        timings["overlay.mount.materialize_lower_s"] = (
            monotonic_now() - materialize_start
        )

    copy_start = monotonic_now()
    _copy_tree(lowerdir, merged)
    if timings is not None:
        timings["overlay.mount.copy_lower_to_merged_s"] = (
            monotonic_now() - copy_start
        )
    return OverlayMountedSnapshot(
        manifest=manifest,
        workspace_root=str(merged),
        lowerdir=str(lowerdir),
        upperdir=str(upperdir),
        workdir=str(workdir),
    )


def cleanup_runtime_run_dir(run_dir: str | Path) -> None:
    """Remove non-load-bearing trees after capture while keeping result refs."""
    run_root = Path(run_dir)
    for name in _INTERMEDIATE_RUN_DIRS:
        shutil.rmtree(run_root / name, ignore_errors=True)


def _copy_tree(source: Path, destination: Path) -> None:
    # ``destination`` is pre-created next to upper/work dirs, so copy each root
    # entry while preserving top-level symlinks and recursively preserving
    # symlinks below directories.
    for entry in source.iterdir():
        target = destination / entry.name
        if entry.is_symlink():
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(entry.readlink())
        elif entry.is_dir():
            shutil.copytree(entry, target, symlinks=True)
        elif entry.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(entry, target)


__all__ = [
    "OverlayMountedSnapshot",
    "cleanup_runtime_run_dir",
    "mount_snapshot",
]
