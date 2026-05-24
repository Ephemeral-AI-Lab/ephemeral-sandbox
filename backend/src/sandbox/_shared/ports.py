"""Mode-agnostic protocol types shared by sandbox workspace pipelines."""

from sandbox._shared.shell_contract import (
    ChangesetResultLike,
    OCCMutationClient,
    SnapshotManifest,
    WorkspaceCapturePublishResult,
    WorkspaceLeaseClient,
    WorkspaceSnapshotLease,
)

__all__ = [
    "ChangesetResultLike",
    "OCCMutationClient",
    "SnapshotManifest",
    "WorkspaceCapturePublishResult",
    "WorkspaceLeaseClient",
    "WorkspaceSnapshotLease",
]
