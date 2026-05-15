"""Public storage facade for sandbox layer-stack state."""

from __future__ import annotations

import logging
import shutil
import tempfile
import threading
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.paths import remove_path, resolve_storage_path
from sandbox.layer_stack.storage_lock import acquire_storage_writer_lock
from sandbox.layer_stack.layer_change import LayerChange
from sandbox.layer_stack.layer_publisher import LayerPublisher
from sandbox.layer_stack.lease import LeaseRegistry, WorkspaceLease
from sandbox.layer_stack.maintenance import SquashService, manifest_still_ends_with
from sandbox.layer_stack.manifest import (
    FileManifestStore,
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    empty_manifest,
    manifest_root_hash,
)
from sandbox.layer_stack.transaction import (
    LayerStackTransaction,
    LayerStackTransactionHandle,
)
from sandbox.layer_stack.view import MergedView, SymlinkLookup
from sandbox.timing import monotonic_now

logger = logging.getLogger(__name__)

_TRANSIENT_LOWERDIR_DIR = "transient-lowerdirs"


def _safe_request_part(value: str) -> str:
    safe = "".join(ch if ch.isalnum() or ch in "-_" else "-" for ch in value)
    return safe[:48] or "request"


def _log_rmtree_failure(func: object, path: object, exc_info: object) -> None:
    """``shutil.rmtree`` onerror callback: surface cleanup leaks via logs."""
    logger.warning(
        "layer-stack cleanup failed: %s(%r) -> %r",
        getattr(func, "__name__", repr(func)),
        path,
        exc_info,
    )


@dataclass(frozen=True)
class CommitStagingArea:
    staging_id: str
    path: Path


@dataclass(frozen=True)
class PrepareWorkspaceSnapshotResult:
    lease_id: str
    manifest_version: int
    root_hash: str
    manifest: Manifest
    lowerdir: str
    timings: dict[str, float]

    def to_dict(self) -> dict[str, object]:
        return {
            "lease_id": self.lease_id,
            "manifest_version": self.manifest_version,
            "root_hash": self.root_hash,
            "manifest": self.manifest.to_dict(),
            "lowerdir": self.lowerdir,
            "timings": dict(self.timings),
        }


class LayerStackManager:
    """Coordinates active manifests, snapshot leases, reads, and publishes."""

    def __init__(
        self,
        storage_root: str | Path,
        *,
        manifest_store: FileManifestStore | None = None,
        leases: LeaseRegistry | None = None,
        view: MergedView | None = None,
        publisher: LayerPublisher | None = None,
        squash: SquashService | None = None,
    ) -> None:
        self.storage_root = Path(storage_root)
        self.storage_root.mkdir(parents=True, exist_ok=True)
        self._storage_writer_lock = acquire_storage_writer_lock(self.storage_root)
        (self.storage_root / LAYERS_DIR).mkdir(exist_ok=True)
        (self.storage_root / STAGING_DIR).mkdir(exist_ok=True)

        self._manifest_store = manifest_store or FileManifestStore(self.storage_root)
        self._manifest_file = self._manifest_store.path
        if not self._manifest_file.exists():
            self._manifest_store.write(empty_manifest())

        self._lock = threading.RLock()
        self._leases = leases or LeaseRegistry()
        self._view = view or MergedView(self.storage_root)
        self._publisher = publisher or LayerPublisher(self.storage_root)
        self._squash = squash or SquashService(self.storage_root)
        self._transaction_handle = LayerStackTransactionHandle(
            lock=self._lock,
            manifest_store=self._manifest_store,
            publisher=self._publisher,
        )

    def read_active_manifest(self) -> Manifest:
        return self._manifest_store.read()

    def acquire_snapshot_lease(self, owner_request_id: str) -> WorkspaceLease:
        with self._lock:
            return self._leases.acquire(
                self.read_active_manifest(),
                owner_request_id,
            )

    def prepare_workspace_snapshot(
        self,
        owner_request_id: str,
    ) -> PrepareWorkspaceSnapshotResult:
        total_start = monotonic_now()
        with self._lock:
            manifest = self._manifest_store.read()
            lease = self._leases.acquire(manifest, owner_request_id)
        lowerdir: Path | None = None
        try:
            lowerdir = (
                self.storage_root
                / "runtime"
                / _TRANSIENT_LOWERDIR_DIR
                / f"{_safe_request_part(owner_request_id)}-{uuid4().hex[:8]}"
                / "lower"
            )
            materialize_start = monotonic_now()
            # share_inodes=True: the lowerdir feeds an overlay read-only mount
            # (or is copy-tree'd into a separate merged dir in copy-backed
            # mode). Sharing inodes with source layers avoids byte copies
            # under concurrent prepare_workspace_snapshot.
            self._view.materialize(lowerdir, manifest, share_inodes=True)
            materialize_elapsed = monotonic_now() - materialize_start
            return PrepareWorkspaceSnapshotResult(
                lease_id=lease.lease_id,
                manifest_version=manifest.version,
                root_hash=manifest_root_hash(manifest),
                manifest=manifest,
                lowerdir=lowerdir.as_posix(),
                timings={
                    "layer_stack.materialize_s": materialize_elapsed,
                    "layer_stack.prepare_workspace_snapshot.total_s": (
                        monotonic_now() - total_start
                    ),
                },
            )
        except Exception:
            if lowerdir is not None:
                # Log cleanup errors instead of swallowing them with
                # ignore_errors=True. A leaked transient lowerdir on every
                # failed snapshot is a slow disk-fill bug; logging surfaces
                # the leak in operator dashboards.
                shutil.rmtree(
                    lowerdir.parent, onerror=_log_rmtree_failure
                )
            with self._lock:
                self._leases.release(lease.lease_id)
            raise

    def release_lease(self, lease_id: str) -> bool:
        with self._lock:
            lease = self._leases.release(lease_id)
            if lease is None:
                return False
            active_manifest = self._manifest_store.read()
            removable = self._unreferenced_layers(
                lease.manifest.layers,
                current_manifest=active_manifest,
            )
        self._remove_layers(removable)
        return True

    def pinned_layers(self) -> tuple[LayerRef, ...]:
        return self._leases.pinned_layers()

    def active_lease_count(self) -> int:
        return self._leases.active_count()

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

    def materialize(self, destination: str | Path, manifest: Manifest | None = None) -> None:
        self._view.materialize(destination, manifest or self.read_active_manifest())

    def commit_transaction(self) -> LayerStackTransaction:
        return LayerStackTransaction(self._transaction_handle)

    def allocate_commit_staging(self, request_id: str) -> CommitStagingArea:
        parent = self.storage_root / STAGING_DIR
        parent.mkdir(parents=True, exist_ok=True)
        path = Path(
            tempfile.mkdtemp(
                prefix=f"occ-commit-{_safe_request_part(request_id)}-",
                dir=str(parent),
            )
        )
        return CommitStagingArea(staging_id=path.name, path=path)

    def drop_commit_staging(self, staging_id: str) -> None:
        if not staging_id:
            return
        shutil.rmtree(self.storage_root / STAGING_DIR / staging_id, ignore_errors=True)

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
        with self._lock:
            active = self._manifest_store.read()
            plan = self._squash.plan(active, max_depth=max_depth)
            if plan is None:
                return None
            squash_lease = self._leases.acquire(
                active,
                f"squash-{uuid4().hex}",
            )

        checkpoint: LayerRef | None = None
        checkpoint_committed = False
        try:
            checkpoint = self._squash.build_checkpoint(plan)
            with self._lock:
                current = self._manifest_store.read()
                if not manifest_still_ends_with(
                    current,
                    plan.suffix_to_checkpoint,
                ):
                    return None
                next_version = current.version + 1
                if not checkpoint.layer_id.startswith(f"B{next_version:06d}-"):
                    checkpoint = self._squash.relabel_checkpoint(
                        checkpoint,
                        manifest_version=next_version,
                    )
                live_prefix = current.layers[: -len(plan.suffix_to_checkpoint)]
                new_manifest = Manifest(
                    version=next_version,
                    layers=(*live_prefix, checkpoint),
                )
                self._manifest_store.write(new_manifest)
                checkpoint_committed = True
            return new_manifest
        finally:
            if checkpoint is not None and not checkpoint_committed:
                self._squash.discard_checkpoint(checkpoint)
            self.release_lease(squash_lease.lease_id)

    def _layer_path(self, layer: LayerRef) -> Path:
        return resolve_storage_path(self.storage_root, layer.path)

    def _unreferenced_layers(
        self,
        candidates: Sequence[LayerRef],
        *,
        current_manifest: Manifest,
    ) -> tuple[LayerRef, ...]:
        skip = set(current_manifest.layers) | set(self._leases.pinned_layers())
        return tuple(layer for layer in candidates if layer not in skip)

    def _remove_layers(self, layers: Sequence[LayerRef]) -> tuple[str, ...]:
        removed: list[str] = []
        for layer in layers:
            remove_path(self._layer_path(layer))
            _layer_digest_path(self.storage_root, layer.layer_id).unlink(missing_ok=True)
            self._view.evict_layer_index(layer.layer_id)
            removed.append(layer.layer_id)
        return tuple(removed)

    def close(self) -> None:
        if self._storage_writer_lock is not None:
            self._storage_writer_lock.close()
            self._storage_writer_lock = None


def _layer_digest_path(storage_root: Path, layer_id: str) -> Path:
    return storage_root / ".layer-metadata" / f"{layer_id}.digest"
