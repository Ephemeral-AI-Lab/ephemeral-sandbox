"""Copy-backed workspace preparation for one snapshot overlay request.

This module intentionally does not enter a Linux mount namespace. The
privileged kernel-overlay entrypoint is
``sandbox.command_exec.entrypoints.namespace_helper``; this portable path
materializes the leased snapshot into a normal directory tree and captures the
diff afterward.
"""

from __future__ import annotations

import shutil
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.view import MergedView
from sandbox.timing import monotonic_now

_LOWERDIR_NAME = "lower"
_UPPERDIR_NAME = "upper"  # Load-bearing: capture refs into this tree after return.
_WORKDIR_NAME = "work"
_MERGED_NAME = "merged"
_INTERMEDIATE_RUN_DIRS: tuple[str, ...] = (_LOWERDIR_NAME, _MERGED_NAME, _WORKDIR_NAME)


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
    lowerdir = run_root / _LOWERDIR_NAME
    upperdir = run_root / _UPPERDIR_NAME
    workdir = run_root / _WORKDIR_NAME
    merged = run_root / _MERGED_NAME
    if timings is None:
        timings = {}

    prepare_start = monotonic_now()
    for directory in (upperdir, workdir, merged):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    timings["overlay.mount.prepare_dirs_s"] = monotonic_now() - prepare_start

    materialize_start = monotonic_now()
    MergedView(storage_root).materialize(lowerdir, manifest)
    timings["overlay.mount.materialize_lower_s"] = monotonic_now() - materialize_start

    copy_start = monotonic_now()
    shutil.copytree(lowerdir, merged, symlinks=True, dirs_exist_ok=True)
    timings["overlay.mount.copy_lower_to_merged_s"] = monotonic_now() - copy_start
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


__all__ = [
    "OverlayMountedSnapshot",
    "cleanup_runtime_run_dir",
    "mount_snapshot",
]
