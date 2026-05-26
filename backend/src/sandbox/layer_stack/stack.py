"""Public storage facade for sandbox layer-stack state."""

from __future__ import annotations

import threading
from contextlib import AbstractContextManager
from collections.abc import Iterator, Sequence
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.paths import remove_path, resolve_storage_path
from sandbox.layer_stack.storage_lock import (
    StorageWriterLockLease,
    acquire_storage_writer_lock,
)
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.commit_staging import (
    CommitStagingArea,
    allocate_commit_staging,
    drop_commit_staging,
)
from sandbox.layer_stack.publisher import LayerPublisher
from sandbox.layer_stack.lease import LeaseRegistry, WorkspaceLease
from sandbox.layer_stack.squash import (
    CheckpointSegment,
    LayerCheckpointSquasher,
    manifest_prefix_before_plan,
)
from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    empty_manifest,
    layer_digest_path,
    manifest_path,
    manifest_root_hash,
    read_manifest,
    write_manifest_atomic,
)
from sandbox.layer_stack.transaction import LayerStackTransaction
from sandbox.layer_stack.view import MergedView, SymlinkLookup
from sandbox.layer_stack.workspace_flush import flush_to_workspace
from sandbox._shared.clock import monotonic_now


@dataclass(frozen=True)
class LayerStackSnapshotLease:
    lease_id: str
    manifest_version: int
    root_hash: str
    manifest: Manifest
    timings: dict[str, float]
    layer_paths: tuple[str, ...]

    def to_dict(self) -> dict[str, object]:
        result: dict[str, object] = {
            "lease_id": self.lease_id,
            "manifest_version": self.manifest_version,
            "root_hash": self.root_hash,
            "manifest": self.manifest.to_dict(),
            "timings": dict(self.timings),
        }
        result["layer_paths"] = list(self.layer_paths)
        return result


# Compatibility for current direct imports; new code should use the lease name.
PrepareWorkspaceSnapshotResult = LayerStackSnapshotLease


class LayerStack:
    """Coordinates active manifests, snapshot leases, reads, and publishes."""

    def __init__(
        self,
        storage_root: str | Path,
    ) -> None:
        self.storage_root = Path(storage_root)
        self.storage_root.mkdir(parents=True, exist_ok=True)
        self._storage_writer_lock: StorageWriterLockLease | None = (
            acquire_storage_writer_lock(self.storage_root)
        )
        (self.storage_root / LAYERS_DIR).mkdir(exist_ok=True)
        (self.storage_root / STAGING_DIR).mkdir(exist_ok=True)

        self._manifest_file = manifest_path(self.storage_root)
        if not self._manifest_file.exists():
            write_manifest_atomic(self._manifest_file, empty_manifest())

        self._lock = threading.RLock()
        self._leases = LeaseRegistry()
        self._view = MergedView(self.storage_root)
        self._publisher = LayerPublisher(self.storage_root)
        self._checkpoint_squasher = LayerCheckpointSquasher(self.storage_root)

    def read_active_manifest(self) -> Manifest:
        return read_manifest(self._manifest_file)

    def acquire_snapshot_lease(self, owner_request_id: str) -> WorkspaceLease:
        with self._lock:
            return self._leases.acquire(
                self.read_active_manifest(),
                owner_request_id,
            )

    def prepare_workspace_snapshot(
        self,
        owner_request_id: str,
    ) -> LayerStackSnapshotLease:
        total_start = monotonic_now()
        with self._lock:
            manifest = self.read_active_manifest()
            lease = self._leases.acquire(manifest, owner_request_id)
        try:
            layer_paths = tuple(
                self._layer_path(layer).as_posix() for layer in manifest.layers
            )
            return LayerStackSnapshotLease(
                lease_id=lease.lease_id,
                manifest_version=manifest.version,
                root_hash=manifest_root_hash(manifest),
                manifest=manifest,
                layer_paths=layer_paths,
                timings={
                    "layer_stack.materialize_s": 0.0,
                    "layer_stack.prepare_workspace_snapshot.total_s": (
                        monotonic_now() - total_start
                    ),
                },
            )
        except Exception:
            with self._lock:
                self._leases.release(lease.lease_id)
            raise

    def release_lease(self, lease_id: str) -> bool:
        with self._storage_write_guard():
            with self._lock:
                lease = self._leases.release(lease_id)
                if lease is None:
                    return False
                active_manifest = self.read_active_manifest()
                removable = self._unreferenced_layers(
                    lease.manifest.layers,
                    current_manifest=active_manifest,
                )
            self._remove_layers(removable)
            return True

    def leased_layers(self) -> tuple[LayerRef, ...]:
        return self._leases.leased_layers()

    def active_lease_count(self) -> int:
        return self._leases.active_count()

    def can_squash(self, *, max_depth: int) -> bool:
        with self._lock:
            active = self.read_active_manifest()
            return (
                self._checkpoint_squasher.plan(
                    active,
                    max_depth=max_depth,
                    barrier_layers=self._leases.lease_head_layers(),
                    min_reduction=2,
                )
                is not None
            )

    def read_bytes(
        self,
        path: str,
        manifest: Manifest | None = None,
    ) -> tuple[bytes | None, bool]:
        return self._view.read_bytes(path, manifest or self.read_active_manifest())

    def read_text(
        self,
        path: str,
        manifest: Manifest | None = None,
    ) -> tuple[str, bool]:
        return self._view.read_text(path, manifest or self.read_active_manifest())

    def read_symlink(
        self,
        path: str,
        manifest: Manifest | None = None,
    ) -> tuple[str, SymlinkLookup]:
        return self._view.read_symlink(path, manifest or self.read_active_manifest())

    def list_dir(
        self,
        path: str = "",
        manifest: Manifest | None = None,
    ) -> tuple[str, ...]:
        return self._view.list_dir(path, manifest or self.read_active_manifest())

    def iter_paths(self, manifest: Manifest | None = None) -> Iterator[str]:
        return self._view.iter_paths(manifest or self.read_active_manifest())

    def materialize(self, destination: str | Path, manifest: Manifest | None = None) -> None:
        self._view.materialize(destination, manifest or self.read_active_manifest())

    def commit_transaction(self) -> LayerStackTransaction:
        storage_writer_lock = self._require_storage_writer_lock()
        return LayerStackTransaction(
            lock=self._lock,
            manifest_path=self._manifest_file,
            publisher=self._publisher,
            storage_writer_lock=storage_writer_lock,
        )

    def allocate_commit_staging(self, request_id: str) -> CommitStagingArea:
        return allocate_commit_staging(self.storage_root, request_id)

    def drop_commit_staging(self, staging_id: str) -> None:
        drop_commit_staging(self.storage_root, staging_id)

    def publish_changes(self, changes: Sequence[LayerChange]) -> Manifest:
        """Publish trusted/test-origin layer changes.

        Production OCC commits publish through :class:`LayerStackTransaction`
        with an explicit staging source root. This direct facade remains for
        tests and trusted storage-maintenance callers that already own the
        source path provenance.
        """
        with self.commit_transaction() as transaction:
            return transaction.publish_layer(changes)

    def squash(self, *, max_depth: int) -> Manifest | None:
        with self._storage_write_guard():
            with self._lock:
                active = self.read_active_manifest()
                plan = self._checkpoint_squasher.plan(
                    active,
                    max_depth=max_depth,
                    barrier_layers=self._leases.lease_head_layers(),
                )
                if plan is None:
                    return None
                squash_lease = self._leases.acquire(
                    active,
                    f"squash-{uuid4().hex}",
                )

            checkpoints: list[LayerRef] = []
            checkpoint_committed = False
            try:
                for segment in plan.checkpoint_segments:
                    checkpoints.append(
                        self._checkpoint_squasher.build_checkpoint(
                            segment,
                            active_version=plan.active_version,
                        )
                    )
                with self._lock:
                    current = self.read_active_manifest()
                    live_prefix = manifest_prefix_before_plan(current, plan)
                    if live_prefix is None:
                        return None
                    next_version = current.version + 1
                    checkpoint_index = 0
                    new_layers = list(live_prefix)
                    for entry in plan.entries:
                        if isinstance(entry, CheckpointSegment):
                            checkpoint = checkpoints[checkpoint_index]
                            if not checkpoint.layer_id.startswith(
                                f"B{next_version:06d}-"
                            ):
                                checkpoint = (
                                    self._checkpoint_squasher.relabel_checkpoint(
                                        checkpoint,
                                        manifest_version=next_version,
                                    )
                                )
                                checkpoints[checkpoint_index] = checkpoint
                            new_layers.append(checkpoint)
                            checkpoint_index += 1
                        else:
                            new_layers.append(entry)
                    new_manifest = Manifest(
                        version=next_version,
                        layers=tuple(new_layers),
                    )
                    write_manifest_atomic(self._manifest_file, new_manifest)
                    checkpoint_committed = True
                return new_manifest
            finally:
                if not checkpoint_committed:
                    for checkpoint in checkpoints:
                        self._checkpoint_squasher.discard_checkpoint(checkpoint)
                self.release_lease(squash_lease.lease_id)

    def flush_to_workspace(
        self,
        *,
        workspace_root: str | Path,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        """Collapse the active manifest back into the bound workspace base.

        The caller must ensure any live overlay mount is detached first. This
        method rewrites the workspace to the active merged view, resets layer
        storage, and rebuilds a fresh base layer from the workspace bytes.
        """
        with self._storage_write_guard():
            result = flush_to_workspace(
                storage_root=self.storage_root,
                workspace_root=workspace_root,
                manifest_path=self._manifest_file,
                view=self._view,
                leases=self._leases,
                lock=self._lock,
                timings=timings,
            )
            self._view = result.view
            self._publisher = result.publisher
            self._checkpoint_squasher = result.checkpoint_squasher
            return result.manifest

    def _storage_write_guard(self) -> AbstractContextManager[object]:
        return self._require_storage_writer_lock().exclusive()

    def _require_storage_writer_lock(self) -> StorageWriterLockLease:
        if self._storage_writer_lock is None:
            raise RuntimeError("layer-stack storage writer lock is closed")
        return self._storage_writer_lock

    def _layer_path(self, layer: LayerRef) -> Path:
        return resolve_storage_path(self.storage_root, layer.path)

    def _unreferenced_layers(
        self,
        candidates: Sequence[LayerRef],
        *,
        current_manifest: Manifest,
    ) -> tuple[LayerRef, ...]:
        skip = set(current_manifest.layers) | set(self._leases.leased_layers())
        return tuple(layer for layer in candidates if layer not in skip)

    def _remove_layers(self, layers: Sequence[LayerRef]) -> None:
        for layer in layers:
            remove_path(self._layer_path(layer))
            layer_digest_path(self.storage_root, layer.layer_id).unlink(missing_ok=True)
            self._view.evict_layer_index(layer.layer_id)

    def close(self) -> None:
        if self._storage_writer_lock is not None:
            self._storage_writer_lock.close()
            self._storage_writer_lock = None
