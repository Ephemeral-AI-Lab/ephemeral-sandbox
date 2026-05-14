"""Capture workspace-relative changes from a command upperdir."""

from __future__ import annotations

from collections.abc import Sequence

from sandbox.command_exec.contract.spec import WorkspaceReplacementMountSpec
from sandbox.overlay import OverlayPathChange
from sandbox.overlay import capture_changes


def capture_workspace_upperdir(
    *,
    spec: WorkspaceReplacementMountSpec,
    mounted_workspace_root: str,
    copy_backed: bool,
    timings: dict[str, float],
) -> Sequence[OverlayPathChange]:
    """Return only assigned-workspace changes for one command."""
    if copy_backed:
        return capture_changes(
            spec.upperdir,
            lowerdir=spec.lowerdir,
            workspace_root=mounted_workspace_root,
            timings=timings,
        )
    return capture_changes(
        spec.upperdir,
        timings=timings,
    )


__all__ = ["capture_workspace_upperdir"]
