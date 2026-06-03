"""Post-publish OCC maintenance policies."""

from __future__ import annotations
import threading
from collections.abc import Callable
from typing import Protocol

from sandbox.layer_stack.manifest import Manifest, manifest_root_hash
from sandbox.occ.changeset import ChangesetResult
from sandbox.occ.ports import LayerSnapshotReader
from sandbox._shared.timing_keys import TimingKey
from sandbox._shared.clock import monotonic_now


class MaintenancePolicy(Protocol):
    """Post-publish maintenance hook for OCC service commits."""

    def after_publish_sync(self, result: ChangesetResult) -> dict[str, float]: ...


class _LayerSquashPort(Protocol):
    """Layer-stack maintenance capability consumed by auto-squash."""

    def can_squash(self, *, max_depth: int) -> bool: ...

    def squash(self, *, max_depth: int) -> Manifest | None: ...


class AutoSquashMaintenancePolicy:
    """Synchronous layer-stack squash after successful publishes."""

    def __init__(
        self,
        *,
        snapshot_reader: LayerSnapshotReader,
        squasher: _LayerSquashPort,
        max_depth: int,
        audit: Callable[..., None] | None = None,
    ) -> None:
        self._snapshot_reader = snapshot_reader
        self._squasher = squasher
        self._max_depth = int(max_depth)
        self._audit = audit
        self._squash_lock = threading.Lock()

    def after_publish_sync(self, result: ChangesetResult) -> dict[str, float]:
        if result.published_manifest_version is None:
            return {}
        active = self._snapshot_reader.read_active_manifest()
        if active.depth <= self._max_depth:
            return {}

        return self._run_squash_for_active(active)

    def _run_squash_for_active(self, active: Manifest) -> dict[str, float]:
        with self._squash_lock:
            active = self._snapshot_reader.read_active_manifest()
            if active.depth <= self._max_depth:
                return {}
            if not self._squasher.can_squash(max_depth=self._max_depth):
                return {}

            self._emit_audit(
                triggered=True,
                trigger_reason="post_publish_depth",
                input_layers=active.depth,
            )

            squash_start = monotonic_now()
            squashed = self._squasher.squash(max_depth=self._max_depth)
            elapsed = monotonic_now() - squash_start
            timings = {
                TimingKey.LAYER_AUTO_SQUASH_TOTAL: elapsed,
                TimingKey.LAYER_AUTO_SQUASH_MAX_DEPTH: float(self._max_depth),
                TimingKey.LAYER_AUTO_SQUASH_DEPTH_BEFORE: float(active.depth),
            }
            if squashed is None:
                timings[TimingKey.LAYER_AUTO_SQUASH_RACED] = 1.0
                self._emit_audit(
                    failed=True,
                    input_layers=active.depth,
                    failure_kind="raced_or_plan_aborted",
                )
                return timings
            timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER] = float(squashed.depth)
            timings[TimingKey.LAYER_AUTO_SQUASH_MANIFEST_VERSION] = float(
                squashed.version
            )
            self._emit_audit(
                completed=True,
                input_layers=active.depth,
                result_layers=squashed.depth,
                manifest_root_hash_value=manifest_root_hash(squashed),
            )
            return timings

    def _emit_audit(self, **payload: object) -> None:
        if self._audit is not None:
            self._audit(**payload)


__all__ = [
    "AutoSquashMaintenancePolicy",
    "MaintenancePolicy",
]
