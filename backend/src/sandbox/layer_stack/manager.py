"""Public storage facade for sandbox layer-stack state."""

from __future__ import annotations

import logging
import os
import shutil
import tempfile
import threading
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.commit.staging import CommitStagingArea
from sandbox.layer_stack._paths import (
    log_rmtree_failure,
    remove_path,
    resolve_storage_path,
    safe_request_part,
)
from sandbox.layer_stack.layer.change import LayerChange
from sandbox.layer_stack.layer.publisher import LayerPublisher
from sandbox.layer_stack.lease.registry import LeaseRegistry, WorkspaceLease
from sandbox.layer_stack.maintenance.squash import SquashWorker, manifest_still_ends_with
from sandbox.layer_stack.manifest import (
    FileManifestStore,
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    empty_manifest,
    manifest_root_hash,
)
from sandbox.layer_stack.protocols import (
    ChangePublisher,
    LeaseStore,
    ManifestStore,
    SnapshotMaterializer,
)
from sandbox.layer_stack.transaction import (
    LayerStackTransaction,
    LayerStackTransactionHandle,
)
from sandbox.layer_stack.view.merged import MergedView
from sandbox.timing import monotonic_now

logger = logging.getLogger(__name__)

_TRANSIENT_LOWERDIR_DIR = "transient-lowerdirs"
_STORAGE_WRITER_LOCK_FILE = ".storage-writer.lock"
_STORAGE_WRITER_LOCKS: dict[str, int] = {}
_STORAGE_WRITER_LOCKS_LOCK = threading.Lock()

try:
    import fcntl
except ImportError:  # pragma: no cover - Windows-only fallback.
    fcntl = None  # type: ignore[assignment]


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

    def __init__(self, storage_root: str | Path) -> None:
        self.storage_root = Path(storage_root)
        self.storage_root.mkdir(parents=True, exist_ok=True)
        self._storage_writer_lock_fd = _acquire_storage_writer_lock(self.storage_root)
        (self.storage_root / LAYERS_DIR).mkdir(exist_ok=True)
        (self.storage_root / STAGING_DIR).mkdir(exist_ok=True)

        self._manifest_file = manifest_path(self.storage_root)
        if not self._manifest_file.exists():
            write_manifest_atomic(self._manifest_file, empty_manifest())

        self._lock = threading.RLock()
        self._leases = LeaseRegistry()
        self._view = MergedView(self.storage_root)
        self._publisher = LayerPublisher(self.storage_root)
        self._squash = SquashWorker(self.storage_root)

    def read_active_manifest(self) -> Manifest:
        with self._lock:
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
    ) -> PrepareWorkspaceSnapshotResult:
        total_start = monotonic_now()
        with self._lock:
            manifest = read_manifest(self._manifest_file)
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
            # link_ok=True: the lowerdir feeds an overlay read-only mount
            # (or is copy-tree'd into a separate merged dir in copy-backed
            # mode). Sharing inodes with source layers avoids byte copies
            # under concurrent prepare_workspace_snapshot.
            self._view.materialize(lowerdir, manifest, link_ok=True)
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
                # WR-01: log cleanup errors instead of swallowing them with
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
            active_manifest = read_manifest(self._manifest_file)
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
    ) -> tuple[str, bool]:
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
        return LayerStackTransaction(self)

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
            active = read_manifest(self._manifest_file)
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
                current = read_manifest(self._manifest_file)
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
                write_manifest_atomic(self._manifest_file, new_manifest)
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
        active_layers = set(current_manifest.layers)
        pinned_layers = set(self._leases.pinned_layers())
        removable: list[LayerRef] = []
        for layer in sorted(set(candidates), key=lambda item: item.layer_id):
            if layer in active_layers or layer in pinned_layers:
                continue
            removable.append(layer)
        return tuple(removable)

    def _remove_layers(self, layers: Sequence[LayerRef]) -> tuple[str, ...]:
        removed: list[str] = []
        for layer in layers:
            remove_path(self._layer_path(layer))
            _layer_digest_path(self.storage_root, layer.layer_id).unlink(missing_ok=True)
            self._view.evict_layer_index(layer.layer_id)
            removed.append(layer.layer_id)
        return tuple(removed)


class LayerStackTransaction:
    """Process-local active-manifest transaction shell."""

    def __init__(self, manager: LayerStackManager) -> None:
        self._manager = manager
        self._manifest: Manifest | None = None
        self._entered = False
        self._lock_acquired_at: float | None = None
        self._lock_held_s = 0.0
        self._lock_wait_s = 0.0

    def __enter__(self) -> LayerStackTransaction:
        wait_start = monotonic_now()
        self._manager._lock.acquire()
        acquired_at = monotonic_now()
        self._lock_wait_s = acquired_at - wait_start
        self._lock_acquired_at = acquired_at
        self._entered = True
        self._manifest = read_manifest(self._manager._manifest_file)
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        del exc_type, exc, traceback
        self._entered = False
        self._manifest = None
        if self._lock_acquired_at is not None:
            self._lock_held_s = monotonic_now() - self._lock_acquired_at
            self._lock_acquired_at = None
        self._manager._lock.release()

    def snapshot(self) -> Manifest:
        return self._require_manifest()

    def publish_layer(
        self,
        changes: Sequence[LayerChange],
        *,
        source_root: str | Path | None = None,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        current = self._require_manifest()
        new_manifest = self._manager._publisher.publish_layer(
            tuple(changes),
            expected_manifest=current,
            source_root=source_root,
            timings=timings,
        )
        self._manifest = new_manifest
        return new_manifest

    @property
    def lock_wait_s(self) -> float:
        return self._lock_wait_s

    @property
    def lock_held_s(self) -> float:
        if self._lock_acquired_at is not None:
            return monotonic_now() - self._lock_acquired_at
        return self._lock_held_s

    def _require_manifest(self) -> Manifest:
        if not self._entered or self._manifest is None:
            raise RuntimeError("layer-stack transaction is not active")
        return self._manifest


def _layer_digest_path(storage_root: Path, layer_id: str) -> Path:
    return storage_root / ".layer-metadata" / f"{layer_id}.digest"


def _acquire_storage_writer_lock(storage_root: Path) -> int | None:
    """Hold a process-wide advisory writer lock for this storage root."""
    if fcntl is None:
        logger.warning(
            "layer-stack storage writer lock unavailable; fcntl is missing",
        )
        return None
    key = str(storage_root.resolve())
    with _STORAGE_WRITER_LOCKS_LOCK:
        fd = _STORAGE_WRITER_LOCKS.get(key)
        if fd is not None:
            return fd

        lock_path = storage_root / _STORAGE_WRITER_LOCK_FILE
        fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            os.close(fd)
            raise RuntimeError(
                "layer-stack storage root is already owned by another process: "
                f"{storage_root}"
            ) from exc
        _STORAGE_WRITER_LOCKS[key] = fd
        return fd


def _safe_request_part(value: str) -> str:
    safe = "".join(ch if ch.isalnum() or ch in "-_" else "-" for ch in value)
    return safe[:48] or "request"


def _log_rmtree_failure(func: object, path: object, exc_info: object) -> None:
    """``shutil.rmtree`` onerror callback: surface cleanup leaks via logs."""
    logger.warning(
        "transient lowerdir cleanup failed: %s(%r) -> %r",
        getattr(func, "__name__", repr(func)),
        path,
        exc_info,
    )
