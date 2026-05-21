"""Public storage facade for sandbox layer-stack state."""

from __future__ import annotations

import logging
import os
import shutil
import tempfile
import threading
from contextlib import AbstractContextManager, nullcontext
from collections.abc import Iterator, Sequence
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.paths import (
    TRANSIENT_LOWERDIR_DIR,
    remove_path,
    resolve_storage_path,
)
from sandbox.layer_stack.storage_lock import acquire_storage_writer_lock
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.publisher import LayerPublisher
from sandbox.layer_stack.lease import LeaseRegistry, WorkspaceLease
from sandbox.layer_stack.squash import (
    CheckpointSegment,
    SquashService,
    manifest_prefix_before_plan,
)
from sandbox.layer_stack.manifest import (
    FileManifestStore,
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    empty_manifest,
    layer_digest_path,
    manifest_root_hash,
)
from sandbox.layer_stack.transaction import LayerStackTransaction
from sandbox.layer_stack.view import MergedView, SymlinkLookup
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox._shared.clock import monotonic_now

logger = logging.getLogger(__name__)


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
    lowerdir: str | None
    timings: dict[str, float]
    layer_paths: tuple[str, ...] | None = None

    def to_dict(self) -> dict[str, object]:
        result: dict[str, object] = {
            "lease_id": self.lease_id,
            "manifest_version": self.manifest_version,
            "root_hash": self.root_hash,
            "manifest": self.manifest.to_dict(),
            "lowerdir": self.lowerdir,
            "timings": dict(self.timings),
        }
        if self.layer_paths is not None:
            result["layer_paths"] = list(self.layer_paths)
        return result


class LayerStack:
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
        *,
        lowerdir_root: str | Path | None = None,
        materialize: bool = True,
    ) -> PrepareWorkspaceSnapshotResult:
        total_start = monotonic_now()
        with self._lock:
            manifest = self._manifest_store.read()
            # Pinning invariant: LeaseRegistry.acquire remains the sole pinning
            # entry for layer dirs against squash GC. materialize=False does NOT
            # bypass this registration — manifest.layers flows through
            # _refcounts.update identically in both branches.
            lease = self._leases.acquire(manifest, owner_request_id)

        if not materialize:
            try:
                layer_paths = tuple(
                    self._layer_path(layer).as_posix() for layer in manifest.layers
                )
                return PrepareWorkspaceSnapshotResult(
                    lease_id=lease.lease_id,
                    manifest_version=manifest.version,
                    root_hash=manifest_root_hash(manifest),
                    manifest=manifest,
                    lowerdir=None,
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

        lowerdir: Path | None = None
        try:
            transient_root = (
                Path(lowerdir_root)
                if lowerdir_root is not None
                else self.storage_root / "runtime" / TRANSIENT_LOWERDIR_DIR
            )
            lowerdir = (
                transient_root
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
        with self._storage_write_guard():
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

    def can_squash(self, *, max_depth: int) -> bool:
        with self._lock:
            active = self._manifest_store.read()
            return (
                self._squash.plan(
                    active,
                    max_depth=max_depth,
                    pinned_layers=self._leases.squash_barrier_layers(),
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
        return LayerStackTransaction(
            lock=self._lock,
            manifest_store=self._manifest_store,
            publisher=self._publisher,
            storage_writer_lock=self._storage_writer_lock,
        )

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
        with self._storage_write_guard():
            with self._lock:
                active = self._manifest_store.read()
                plan = self._squash.plan(
                    active,
                    max_depth=max_depth,
                    pinned_layers=self._leases.squash_barrier_layers(),
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
                        self._squash.build_checkpoint(
                            segment,
                            active_version=plan.active_version,
                        )
                    )
                with self._lock:
                    current = self._manifest_store.read()
                    live_prefix = manifest_prefix_before_plan(current, plan)
                    if live_prefix is None:
                        return None
                    next_version = current.version + 1
                    checkpoint_index = 0
                    new_layers = list(live_prefix)
                    for entry in plan.entries:
                        if isinstance(entry, CheckpointSegment):
                            checkpoint = checkpoints[checkpoint_index]
                            if not checkpoint.layer_id.startswith(f"B{next_version:06d}-"):
                                checkpoint = self._squash.relabel_checkpoint(
                                    checkpoint,
                                    manifest_version=next_version,
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
                    self._manifest_store.write(new_manifest)
                    checkpoint_committed = True
                return new_manifest
            finally:
                if not checkpoint_committed:
                    for checkpoint in checkpoints:
                        self._squash.discard_checkpoint(checkpoint)
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
            total_start = monotonic_now()
            workspace = Path(workspace_root)
            if not workspace.is_dir():
                raise ValueError(f"workspace_root does not exist: {workspace}")
            with self._lock:
                if self._leases.active_count() > 0:
                    raise RuntimeError("flush_to_workspace blocked by active leases")
                active = self._manifest_store.read()

            materialize_parent = self.storage_root / "runtime" / "flush"
            materialize_parent.mkdir(parents=True, exist_ok=True)
            materialized = Path(
                tempfile.mkdtemp(prefix="merged-", dir=str(materialize_parent))
            )
            try:
                materialize_start = monotonic_now()
                self._view.materialize(materialized, active, share_inodes=False)
                if timings is not None:
                    timings["layer_stack.flush.materialize_s"] = (
                        monotonic_now() - materialize_start
                    )

                replace_start = monotonic_now()
                _replace_directory_contents(workspace, materialized)
                if timings is not None:
                    timings["layer_stack.flush.replace_workspace_s"] = (
                        monotonic_now() - replace_start
                    )

                reset_start = monotonic_now()
                with self._lock:
                    _clear_storage_root_for_flush(self.storage_root)
                    build_workspace_base(
                        workspace_root=workspace,
                        layer_stack_root=self.storage_root,
                    )
                    self._view = MergedView(self.storage_root)
                    self._publisher = LayerPublisher(self.storage_root)
                    self._squash = SquashService(self.storage_root)
                    new_manifest = self._manifest_store.read()
                if timings is not None:
                    timings["layer_stack.flush.rebuild_base_s"] = (
                        monotonic_now() - reset_start
                    )
                    timings["layer_stack.flush.total_s"] = monotonic_now() - total_start
                return new_manifest
            finally:
                shutil.rmtree(materialized, ignore_errors=True)

    def _storage_write_guard(self) -> AbstractContextManager[object]:
        if self._storage_writer_lock is None:
            return nullcontext()
        return self._storage_writer_lock.exclusive()

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
            layer_digest_path(self.storage_root, layer.layer_id).unlink(missing_ok=True)
            self._view.evict_layer_index(layer.layer_id)
            removed.append(layer.layer_id)
        return tuple(removed)

    def close(self) -> None:
        if self._storage_writer_lock is not None:
            self._storage_writer_lock.close()
            self._storage_writer_lock = None


def _replace_directory_contents(destination: Path, source: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for child in destination.iterdir():
        remove_path(child)
    for child in source.iterdir():
        os.replace(child, destination / child.name)


def _clear_storage_root_for_flush(storage_root: Path) -> None:
    storage_root.mkdir(parents=True, exist_ok=True)
    for child in storage_root.iterdir():
        if child.name == ".storage-writer.lock":
            continue
        remove_path(child)
