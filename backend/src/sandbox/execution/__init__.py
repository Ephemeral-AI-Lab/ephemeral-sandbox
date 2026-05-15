"""Facade for guarded command execution."""

from sandbox.execution.contract import (
    CommandExecRequest,
    CommandExecResult,
    CommandExecutor,
    MountMode,
    OCCMutationClient,
    ShellProcessResult,
    SnapshotManifest,
    WorkspaceCapture,
    WorkspaceLeaseClient,
    WorkspaceReplacementMountSpec,
    WorkspaceSnapshotLease,
)
from sandbox.execution.orchestrator import execute_command, run_workspace_replaced_command
from sandbox.execution.policy import DEFAULT_COMMAND_EXEC_POLICY, CommandExecPolicy

__all__ = [
    "CommandExecPolicy",
    "CommandExecRequest",
    "CommandExecResult",
    "CommandExecutor",
    "DEFAULT_COMMAND_EXEC_POLICY",
    "MountMode",
    "OCCMutationClient",
    "ShellProcessResult",
    "SnapshotManifest",
    "WorkspaceCapture",
    "WorkspaceLeaseClient",
    "WorkspaceReplacementMountSpec",
    "WorkspaceSnapshotLease",
    "execute_command",
    "run_workspace_replaced_command",
]
