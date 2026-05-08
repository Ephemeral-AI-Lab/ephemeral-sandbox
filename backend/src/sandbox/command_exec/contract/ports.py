"""Client protocols consumed by guarded command execution."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Protocol

from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult


class WorkspaceSnapshotLease(Protocol):
    lease_id: str
    manifest_version: int
    manifest: object
    lowerdir: str
    timings: dict[str, float]


class WorkspaceLeaseClient(Protocol):
    """Layer-stack lease/snapshot client used by command execution."""

    def prepare_workspace_snapshot(
        self,
        *,
        workspace_ref: str,
        request_id: str,
    ) -> WorkspaceSnapshotLease: ...

    def release_lease(self, *, workspace_ref: str, lease_id: str) -> bool: ...


class OCCMutationClient(Protocol):
    """OCC mutation client used for shell-capture submission."""

    async def apply_changeset(
        self,
        typed_changes: Sequence[Change],
        *,
        snapshot: Any = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
    ) -> ChangesetResult | PreparedChangeset: ...


__all__ = [
    "OCCMutationClient",
    "WorkspaceLeaseClient",
    "WorkspaceSnapshotLease",
]
