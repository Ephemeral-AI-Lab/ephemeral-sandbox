"""Public storage facade for sandbox layer-stack state."""

from __future__ import annotations

import errno
import os
import shutil
import tempfile
import threading
from contextlib import AbstractContextManager
from collections.abc import Iterator, Sequence
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.paths import remove_path, resolve_safe_storage_path
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
from sandbox.layer_stack.lease import LeaseRegistry, LayerStackLeaseRecord
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
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.shared.clock import monotonic_now, record_elapsed


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

    def acquire_lease_record(self, owner_request_id: str) -> LayerStackLeaseRecord:
        with self._lock:
            return self._leases.acquire(
                self.read_active_manifest(),
                owner_request_id,
            )

    def acquire_snapshot(
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
                    "layer_stack.acquire_snapshot.total_s": (
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
                    lease_head_layers=self._leases.lease_head_layers(),
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

    def project(self, destination: str | Path, manifest: Manifest | None = None) -> None:
        self._view.project(destination, manifest or self.read_active_manifest())

    def begin_transaction(self) -> LayerStackTransaction:
        """Open a new active-manifest publish transaction.

        The returned context manager holds the storage-writer guard + process
        RLock for its lifetime and exposes :meth:`LayerStackTransaction.publish_layer`
        as the publish primitive. Does NOT commit on its own.
        """
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
        with self.begin_transaction() as transaction:
            return transaction.publish_layer(changes)

    def squash(self, *, max_depth: int) -> Manifest | None:
        with self._storage_write_guard():
            with self._lock:
                active = self.read_active_manifest()
                plan = self._checkpoint_squasher.plan(
                    active,
                    max_depth=max_depth,
                    lease_head_layers=self._leases.lease_head_layers(),
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

    def commit_to_workspace(
        self,
        *,
        workspace_root: str | Path,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        """Collapse the active manifest back into the bound workspace base.

        The caller must ensure any live overlay mount is detached first. This
        method rewrites the workspace to the active manifest's projection,
        resets layer storage, and rebuilds a fresh base layer from the
        workspace bytes. Refuses to run while any snapshot lease is active.
        """
        with self._storage_write_guard():
            total_start = monotonic_now()
            workspace = Path(workspace_root)
            if not workspace.is_dir():
                raise ValueError(f"workspace_root does not exist: {workspace}")
            with self._lock:
                if self._leases.active_count() > 0:
                    raise RuntimeError("commit_to_workspace blocked by active leases")
                active = self.read_active_manifest()

            projection_parent = self.storage_root / "runtime" / "commit"
            projection_parent.mkdir(parents=True, exist_ok=True)
            projected = Path(tempfile.mkdtemp(prefix="projected-", dir=str(projection_parent)))
            try:
                project_start = monotonic_now()
                self._view.project(projected, active, share_inodes=False)
                record_elapsed(
                    timings, "layer_stack.commit_to_workspace.project_s", project_start
                )

                replace_start = monotonic_now()
                _replace_workspace_contents(workspace, projected)
                record_elapsed(
                    timings,
                    "layer_stack.commit_to_workspace.replace_workspace_s",
                    replace_start,
                )

                reset_start = monotonic_now()
                with self._lock:
                    _clear_storage_root_preserving_lock(self.storage_root)
                    build_workspace_base(
                        workspace_root=workspace,
                        layer_stack_root=self.storage_root,
                    )
                    self._view = MergedView(self.storage_root)
                    self._publisher = LayerPublisher(self.storage_root)
                    self._checkpoint_squasher = LayerCheckpointSquasher(self.storage_root)
                    new_manifest = self.read_active_manifest()
                record_elapsed(
                    timings,
                    "layer_stack.commit_to_workspace.rebuild_base_s",
                    reset_start,
                )
                record_elapsed(
                    timings, "layer_stack.commit_to_workspace.total_s", total_start
                )
                return new_manifest
            finally:
                shutil.rmtree(projected, ignore_errors=True)

    def _storage_write_guard(self) -> AbstractContextManager[object]:
        return self._require_storage_writer_lock().exclusive()

    def _require_storage_writer_lock(self) -> StorageWriterLockLease:
        if self._storage_writer_lock is None:
            raise RuntimeError("layer-stack storage writer lock is closed")
        return self._storage_writer_lock

    def _layer_path(self, layer: LayerRef) -> Path:
        return resolve_safe_storage_path(self.storage_root, layer.path)

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


def _replace_workspace_contents(destination: Path, source: Path) -> None:
    """Atomically swap *destination*'s children for *source*'s children.

    Falls back to ``shutil.move`` on EXDEV; docker bind-mounts /testbed as
    a separate volume so a kernel rename across the device boundary fails.
    """
    destination.mkdir(parents=True, exist_ok=True)
    for child in destination.iterdir():
        remove_path(child)
    for child in source.iterdir():
        target = destination / child.name
        try:
            os.replace(child, target)
        except OSError as exc:
            if exc.errno != errno.EXDEV:
                raise
            shutil.move(str(child), str(target))


def _clear_storage_root_preserving_lock(storage_root: Path) -> None:
    """Reset *storage_root* contents but keep the writer-lock file in place."""
    storage_root.mkdir(parents=True, exist_ok=True)
    for child in storage_root.iterdir():
        if child.name == ".storage-writer.lock":
            continue
        remove_path(child)
