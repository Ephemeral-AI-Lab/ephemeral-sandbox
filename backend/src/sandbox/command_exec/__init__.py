"""Facade for guarded command execution."""

from __future__ import annotations

from sandbox.command_exec.contract import (
    CommandExecRequest,
    CommandExecResult,
    MountMode,
    OCCMutationClient,
    ShellProcessResult,
    WorkspaceCapture,
    WorkspaceLeaseClient,
    WorkspaceReplacementMountSpec,
    WorkspaceSnapshotLease,
)
from sandbox.command_exec.workspace.capture import capture_workspace_upperdir
from sandbox.command_exec.workspace.mount import run_workspace_replaced_command

__all__ = [
    "CommandExecRequest",
    "CommandExecResult",
    "MountMode",
    "OCCMutationClient",
    "ShellProcessResult",
    "WorkspaceCapture",
    "WorkspaceLeaseClient",
    "WorkspaceReplacementMountSpec",
    "WorkspaceSnapshotLease",
    "capture_workspace_upperdir",
    "run_workspace_replaced_command",
]
