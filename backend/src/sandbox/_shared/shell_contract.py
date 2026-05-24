"""Shell pipeline contract values: request, result, and ports."""

from __future__ import annotations

import os
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from sandbox.overlay.path_change import OverlayPathChange
    from sandbox.occ.changeset import Change, CommitOptions, FileResult

PRIVATE_NAMESPACE_MOUNT = "private_namespace"


# ---- request ---------------------------------------------------------------


@dataclass
class CommandExecRequest:
    """One shell command against a workspace replacement mount."""

    invocation_id: str
    workspace_ref: str
    workspace_root: str
    command: tuple[str, ...]
    cwd: str = "."
    env: Mapping[str, str] = field(default_factory=dict)
    timeout_seconds: float | None = None
    agent_id: str = ""
    description: str = "shell"

    def __post_init__(self) -> None:
        invocation_id = str(self.invocation_id).strip()
        if not invocation_id:
            raise ValueError("invocation_id must not be empty")
        workspace_ref = str(self.workspace_ref).strip()
        if not workspace_ref:
            raise ValueError("workspace_ref must not be empty")
        workspace_root = str(self.workspace_root).strip()
        if not workspace_root.startswith("/"):
            raise ValueError("workspace_root must be an absolute path")
        command = tuple(str(part) for part in self.command)
        if not command or any(part == "" for part in command):
            raise ValueError("command must contain non-empty argv parts")
        if self.timeout_seconds is not None and self.timeout_seconds <= 0:
            raise ValueError("timeout_seconds must be positive when provided")

        cwd_raw = str(self.cwd).strip() or "."
        cwd_normalized = os.path.normpath(cwd_raw)
        if cwd_normalized == ".." or cwd_normalized.startswith("../"):
            raise ValueError(f"cwd must not escape workspace root: {cwd_raw!r}")
        if not cwd_normalized.startswith("/") and ".." in cwd_normalized.split("/"):
            raise ValueError(f"cwd must not contain '..' segments: {cwd_raw!r}")

        self.invocation_id = invocation_id
        self.workspace_ref = workspace_ref
        self.workspace_root = workspace_root.rstrip("/") or "/"
        self.command = command
        self.cwd = cwd_normalized
        self.env = {str(key): str(value) for key, value in self.env.items()}
        self.agent_id = str(self.agent_id)
        self.description = str(self.description or "shell")


# ---- result ----------------------------------------------------------------


@dataclass
class ShellProcessResult:
    """Raw process result and capture locations."""

    exit_code: int
    stdout_ref: str
    stderr_ref: str
    mounted_workspace_root: str
    mount_mode: str

    def __post_init__(self) -> None:
        self.mount_mode = str(self.mount_mode)


# ---- ports -----------------------------------------------------------------


class SnapshotManifest(Protocol):
    """Snapshot manifest shape needed by command execution."""

    version: int
    layers: tuple[object, ...]


class WorkspaceSnapshotLease(Protocol):
    lease_id: str
    manifest_version: int
    manifest: SnapshotManifest
    layer_paths: tuple[str, ...] | None
    timings: Mapping[str, float]


class WorkspaceLeaseClient(Protocol):
    """Layer-stack lease/snapshot client used by command execution."""

    storage_root: Path

    def prepare_workspace_snapshot(
        self,
        *,
        request_id: str,
    ) -> WorkspaceSnapshotLease: ...

    def release_lease(self, *, lease_id: str) -> bool: ...


class OCCMutationClient(Protocol):
    """OCC mutation client used for shell-capture submission."""

    async def apply_changeset(
        self,
        typed_changes: Sequence[Change],
        *,
        snapshot: SnapshotManifest | None = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
        run_maintenance: bool = True,
    ) -> ChangesetResultLike: ...

    async def run_maintenance_after_publish(
        self,
        result: ChangesetResultLike,
        *,
        workspace_ref: str | None = None,
    ) -> dict[str, float]: ...


class ChangesetResultLike(Protocol):
    """Minimal committed changeset result shape consumed by command execution."""

    files: Sequence[FileResult]
    timings: Mapping[str, float]
    published_manifest_version: int | None

    @property
    def success(self) -> bool: ...


@dataclass(frozen=True)
class WorkspaceCapturePublishResult:
    """Result returned by the daemon-owned overlay publish facade."""

    path_changes: Sequence[OverlayPathChange]
    changeset: ChangesetResultLike
    timings: Mapping[str, float] = field(default_factory=dict)

__all__ = [
    "ChangesetResultLike",
    "CommandExecRequest",
    "OCCMutationClient",
    "PRIVATE_NAMESPACE_MOUNT",
    "ShellProcessResult",
    "SnapshotManifest",
    "WorkspaceCapturePublishResult",
    "WorkspaceLeaseClient",
    "WorkspaceSnapshotLease",
]
