"""Public storage facade for sandbox layer-stack state."""

from __future__ import annotations

import threading
import time
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
import shutil
from types import TracebackType

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.lease_registry import Lease, LeaseRegistry
from sandbox.layer_stack.lease_budget import BudgetDecision, LeaseBudgetWorker, LeaseSnapshot
from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    empty_manifest,
    manifest_path,
    read_manifest,
    write_manifest_atomic,
)
from sandbox.layer_stack.merged_view import MergedView
from sandbox.layer_stack.publisher import LayerPublisher
from sandbox.layer_stack.squash import SquashPlan, SquashWorker, manifest_still_ends_with


@dataclass(frozen=True)
class GCMarkSet:
    active_layers: tuple[LayerRef, ...]
    leased_layers: tuple[LayerRef, ...]
    young_staging_dirs: tuple[Path, ...]

    def __post_init__(self) -> None:
        object.__setattr__(self, "active_layers", tuple(self.active_layers))
        object.__setattr__(self, "leased_layers", tuple(self.leased_layers))
        object.__setattr__(
            self,
            "young_staging_dirs",
            tuple(self.young_staging_dirs),
        )


@dataclass(frozen=True)
class FsckResult:
    orphan_layers_removed: tuple[str, ...] = ()
    orphan_staging_removed: tuple[str, ...] = ()
    missing_active_layers: tuple[LayerRef, ...] = ()
    missing_leased_layers: tuple[LayerRef, ...] = ()


class LayerStackManager:
    """Coordinates active manifests, snapshot leases, reads, and publishes."""

    def __init__(
        self,
        storage_root: str | Path,
        *,
        lease_budget: LeaseBudgetWorker | None = None,
    ) -> None:
        self.storage_root = Path(storage_root)
        self.storage_root.mkdir(parents=True, exist_ok=True)
        (self.storage_root / LAYERS_DIR).mkdir(exist_ok=True)
        (self.storage_root / STAGING_DIR).mkdir(exist_ok=True)

        self._manifest_file = manifest_path(self.storage_root)
        if not self._manifest_file.exists():
            write_manifest_atomic(self._manifest_file, empty_manifest())

        self._lock = threading.RLock()
        self._leases = LeaseRegistry()
        self._view = MergedView(self.storage_root)
        self._lease_budget = lease_budget or LeaseBudgetWorker()
        self._publisher = LayerPublisher(
            self.storage_root,
            self._manifest_file,
            backpressure_checker=self._publish_budget_decision,
        )
        self._squash = SquashWorker(self.storage_root, merged_view=self._view)

    def read_active_manifest(self) -> Manifest:
        with self._lock:
            return read_manifest(self._manifest_file)

    def acquire_snapshot_lease(self, owner_id: str) -> Lease:
        with self._lock:
            return self._leases.acquire(self.read_active_manifest(), owner_id)

    def release_lease(self, lease_id: str) -> bool:
        with self._lock:
            return self._leases.release(lease_id) is not None

    def expire_leases_older_than(
        self,
        max_age_seconds: float,
        *,
        now: float | None = None,
    ) -> tuple[Lease, ...]:
        with self._lock:
            return self._leases.expire_older_than(max_age_seconds, now=now)

    def sweep_dead_lease_owners(self, live_owner_ids: Sequence[str]) -> tuple[Lease, ...]:
        with self._lock:
            return self._leases.sweep_dead_owners(live_owner_ids)

    def lease_refcount(self, layer: LayerRef) -> int:
        return self._leases.refcount(layer)

    def pinned_layers(self) -> tuple[LayerRef, ...]:
        return self._leases.pinned_layers()

    def lease_snapshots(self) -> tuple[LeaseSnapshot, ...]:
        with self._lock:
            return tuple(
                LeaseSnapshot(
                    lease_id=lease.lease_id,
                    owner_id=lease.owner_id,
                    manifest_version=lease.manifest.version,
                    pinned_layers=lease.manifest.layers,
                    pinned_bytes=sum(self._layer_size(layer) for layer in lease.manifest.layers),
                    acquired_at=lease.acquired_at,
                )
                for lease in self._leases.active_leases()
            )

    def evaluate_lease_budget(self) -> BudgetDecision:
        with self._lock:
            return self._lease_budget.evaluate(
                active_depth=read_manifest(self._manifest_file).depth,
                snapshots=self.lease_snapshots(),
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

    def commit_transaction(self) -> "LayerStackTransaction":
        return LayerStackTransaction(self)

    def publish_changes(self, changes: Sequence[LayerChange]) -> Manifest:
        with self.commit_transaction() as transaction:
            return transaction.publish_layer(changes)

    def squash_plan(self, *, max_depth: int) -> SquashPlan | None:
        return self._squash.plan(self.read_active_manifest(), max_depth=max_depth)

    def squash(self, *, max_depth: int, collect_garbage: bool = True) -> Manifest | None:
        plan = self.squash_plan(max_depth=max_depth)
        if plan is None:
            return None

        checkpoint = self._squash.build_checkpoint(plan)
        checkpoint_committed = False
        try:
            with self._lock:
                current = read_manifest(self._manifest_file)
                if not manifest_still_ends_with(
                    current,
                    plan.suffix_to_checkpoint,
                ):
                    return None
                live_prefix = current.layers[: -len(plan.suffix_to_checkpoint)]
                new_manifest = Manifest(
                    version=current.version + 1,
                    layers=(*live_prefix, checkpoint),
                )
                write_manifest_atomic(self._manifest_file, new_manifest)
                checkpoint_committed = True
            if collect_garbage:
                self.collect_garbage()
            return new_manifest
        finally:
            if not checkpoint_committed:
                self._squash.discard_checkpoint(checkpoint)

    def build_gc_mark_set(
        self,
        *,
        young_staging_age_seconds: float = 300.0,
        now: float | None = None,
    ) -> GCMarkSet:
        with self._lock:
            return self._build_gc_mark_set(
                young_staging_age_seconds=young_staging_age_seconds,
                now=now,
            )

    def collect_garbage(
        self,
        *,
        young_staging_age_seconds: float = 300.0,
        now: float | None = None,
    ) -> FsckResult:
        with self._lock:
            marks = self._build_gc_mark_set(
                young_staging_age_seconds=young_staging_age_seconds,
                now=now,
            )
            kept_layer_paths = {
                self._layer_path(layer).resolve(strict=False)
                for layer in (*marks.active_layers, *marks.leased_layers)
            }
            removed_layers: list[str] = []
            layers_dir = self.storage_root / LAYERS_DIR
            for child in sorted(layers_dir.iterdir(), key=lambda item: item.name):
                if child.resolve(strict=False) in kept_layer_paths:
                    continue
                _remove_path(child)
                removed_layers.append(child.name)

            young_staging_paths = {
                staging.resolve(strict=False) for staging in marks.young_staging_dirs
            }
            removed_staging: list[str] = []
            staging_dir = self.storage_root / STAGING_DIR
            for child in sorted(staging_dir.iterdir(), key=lambda item: item.name):
                if child.resolve(strict=False) in young_staging_paths:
                    continue
                _remove_path(child)
                removed_staging.append(child.name)

            return FsckResult(
                orphan_layers_removed=tuple(removed_layers),
                orphan_staging_removed=tuple(removed_staging),
                missing_active_layers=self._missing_layers(marks.active_layers),
                missing_leased_layers=self._missing_layers(marks.leased_layers),
            )

    def fsck_cleanup(
        self,
        *,
        young_staging_age_seconds: float = 300.0,
        now: float | None = None,
    ) -> FsckResult:
        return self.collect_garbage(
            young_staging_age_seconds=young_staging_age_seconds,
            now=now,
        )

    def _publish_budget_decision(self, active: Manifest) -> BudgetDecision:
        return self._lease_budget.evaluate(
            active_depth=active.depth,
            snapshots=self.lease_snapshots(),
        )

    def _build_gc_mark_set(
        self,
        *,
        young_staging_age_seconds: float,
        now: float | None,
    ) -> GCMarkSet:
        timestamp = time.time() if now is None else now
        active_layers = read_manifest(self._manifest_file).layers
        return GCMarkSet(
            active_layers=active_layers,
            leased_layers=self._leases.pinned_layers(),
            young_staging_dirs=self._young_staging_dirs(
                now=timestamp,
                young_staging_age_seconds=young_staging_age_seconds,
            ),
        )

    def _young_staging_dirs(
        self,
        *,
        now: float,
        young_staging_age_seconds: float,
    ) -> tuple[Path, ...]:
        if young_staging_age_seconds < 0:
            raise ValueError("young_staging_age_seconds must be non-negative")
        staging_root = self.storage_root / STAGING_DIR
        young: list[Path] = []
        for child in sorted(staging_root.iterdir(), key=lambda item: item.name):
            try:
                age = now - child.stat().st_mtime
            except FileNotFoundError:
                continue
            if age < young_staging_age_seconds:
                young.append(child)
        return tuple(young)

    def _missing_layers(self, layers: Sequence[LayerRef]) -> tuple[LayerRef, ...]:
        missing: list[LayerRef] = []
        for layer in layers:
            if not self._layer_path(layer).is_dir():
                missing.append(layer)
        return tuple(missing)

    def _layer_path(self, layer: LayerRef) -> Path:
        path = Path(layer.path)
        if not path.is_absolute():
            path = self.storage_root / path
        return path

    def _layer_size(self, layer: LayerRef) -> int:
        layer_dir = self._layer_path(layer)
        if not layer_dir.exists():
            return 0
        total = 0
        for entry in layer_dir.rglob("*"):
            if not entry.is_file() and not entry.is_symlink():
                continue
            try:
                total += entry.lstat().st_size
            except FileNotFoundError:
                continue
        return total


class LayerStackTransaction:
    """Process-local active-manifest transaction shell."""

    def __init__(self, manager: LayerStackManager) -> None:
        self._manager = manager
        self._manifest: Manifest | None = None
        self._entered = False
        self._lock_acquired_at: float | None = None
        self._lock_held_s = 0.0
        self._lock_wait_s = 0.0

    def __enter__(self) -> "LayerStackTransaction":
        wait_start = time.perf_counter()
        self._manager._lock.acquire()
        acquired_at = time.perf_counter()
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
            self._lock_held_s = time.perf_counter() - self._lock_acquired_at
            self._lock_acquired_at = None
        self._manager._lock.release()

    def snapshot(self) -> Manifest:
        return self._require_manifest()

    def publish_layer(
        self,
        changes: Sequence[LayerChange],
        *,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        current = self._require_manifest()
        new_manifest = self._manager._publisher.publish_layer_locked(
            tuple(changes),
            expected_manifest=current,
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
            return time.perf_counter() - self._lock_acquired_at
        return self._lock_held_s

    def _require_manifest(self) -> Manifest:
        if not self._entered or self._manifest is None:
            raise RuntimeError("layer-stack transaction is not active")
        return self._manifest


def _remove_path(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink(missing_ok=True)
    elif path.is_dir():
        shutil.rmtree(path)
