"""Mount a frozen layer-stack manifest into a per-call workspace view."""

from __future__ import annotations

import shutil
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.merged_view import MergedView


@dataclass(frozen=True)
class MountedSnapshot:
    manifest: Manifest
    workspace_root: str
    upperdir: str
    workdir: str


def mount_snapshot(
    *,
    manifest: Manifest,
    storage_root: str | Path,
    run_dir: str | Path,
) -> MountedSnapshot:
    """Create a runtime-local merged workspace for a leased manifest.

    The layer stack remains immutable. This function materializes the leased
    manifest into a lowerdir and prepares per-call upper/work/merged dirs. The
    copy-backed merged view keeps unit runs portable; a kernel overlay mount can
    replace this implementation behind the same return object later.
    """
    run_root = Path(run_dir)
    lowerdir = run_root / "lower"
    upperdir = run_root / "upper"
    workdir = run_root / "work"
    merged = run_root / "merged"

    for directory in (upperdir, workdir, merged):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)

    MergedView(storage_root).materialize(lowerdir, manifest)
    _copy_tree(lowerdir, merged)
    return MountedSnapshot(
        manifest=manifest,
        workspace_root=str(merged),
        upperdir=str(upperdir),
        workdir=str(workdir),
    )


def lowerdir_for(mounted: MountedSnapshot) -> str:
    return str(Path(mounted.workdir).parent / "lower")


def _copy_tree(source: Path, destination: Path) -> None:
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
    "MountedSnapshot",
    "lowerdir_for",
    "mount_snapshot",
]
