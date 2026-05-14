"""Internal collaborator protocols for layer-stack orchestration."""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path
from typing import Protocol

from sandbox.layer_stack.commit import CommitStagingArea
from sandbox.layer_stack.layer.change import LayerChange
from sandbox.layer_stack.lease import WorkspaceLease
from sandbox.layer_stack.manifest import LayerRef, Manifest


class ManifestStore(Protocol):
    @property
    def path(self) -> Path: ...

    def read(self) -> Manifest: ...

    def write(self, manifest: Manifest) -> None: ...


class SnapshotMaterializer(Protocol):
    def read_bytes(self, path: str, manifest: Manifest) -> tuple[bytes | None, bool]: ...

    def read_text(self, path: str, manifest: Manifest) -> tuple[str, bool]: ...

    def read_symlink(self, path: str, manifest: Manifest) -> tuple[str, bool]: ...

    def list_dir(self, path: str, manifest: Manifest) -> tuple[str, ...]: ...

    def materialize(
        self,
        destination: str | Path,
        manifest: Manifest,
        *,
        share_inodes: bool = False,
    ) -> None: ...

    def evict_layer_index(self, layer_id: str) -> None: ...


class ChangePublisher(Protocol):
    def publish_layer(
        self,
        changes: Sequence[LayerChange],
        *,
        expected_manifest: Manifest,
        source_root: str | Path | None = None,
        timings: dict[str, float] | None = None,
    ) -> Manifest: ...


class LeaseStore(Protocol):
    def acquire(self, manifest: Manifest, owner_request_id: str) -> WorkspaceLease: ...

    def release(self, lease_id: str) -> WorkspaceLease | None: ...

    def pinned_layers(self) -> tuple[LayerRef, ...]: ...

    def active_count(self) -> int: ...


class CommitStagingStore(Protocol):
    def allocate_commit_staging(self, request_id: str) -> CommitStagingArea: ...

    def drop_commit_staging(self, staging_id: str) -> None: ...


__all__ = [
    "ChangePublisher",
    "CommitStagingStore",
    "LeaseStore",
    "ManifestStore",
    "SnapshotMaterializer",
]
