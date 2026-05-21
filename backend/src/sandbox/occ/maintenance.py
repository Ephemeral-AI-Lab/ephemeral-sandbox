"""Post-publish OCC maintenance policies."""

from __future__ import annotations
from typing import Protocol, runtime_checkable

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset import ChangesetResult
from sandbox.occ.ports import LayerSnapshotReader
from sandbox._shared.timing_keys import TimingKey
from sandbox._shared.clock import monotonic_now


class MaintenancePolicy(Protocol):
    """Post-publish maintenance hook for OCC service commits."""

    def after_publish_sync(self, result: ChangesetResult) -> dict[str, float]: ...


@runtime_checkable
class SquashPort(Protocol):
    """Layer-stack maintenance capability consumed by auto-squash."""

    def squash(self, *, max_depth: int) -> Manifest | None: ...


class AutoSquashMaintenancePolicy:
    """Synchronous layer-stack squash after successful publishes."""

    def __init__(
        self,
        *,
        snapshot_reader: LayerSnapshotReader,
        squasher: SquashPort,
        max_depth: int,
    ) -> None:
        self._snapshot_reader = snapshot_reader
        self._squasher = squasher
        self._max_depth = int(max_depth)

    def after_publish_sync(self, result: ChangesetResult) -> dict[str, float]:
        if result.published_manifest_version is None:
            return {}
        active = self._snapshot_reader.read_active_manifest()
        if active.depth <= self._max_depth:
            return {}

        return self._run_squash_for_active(active)

    def _run_squash_for_active(self, active: Manifest) -> dict[str, float]:
        can_squash = getattr(self._squasher, "can_squash", None)
        if callable(can_squash) and not can_squash(max_depth=self._max_depth):
            return {}

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
            return timings
        timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER] = float(squashed.depth)
        timings[TimingKey.LAYER_AUTO_SQUASH_MANIFEST_VERSION] = float(squashed.version)
        return timings


__all__ = [
    "AutoSquashMaintenancePolicy",
    "MaintenancePolicy",
    "SquashPort",
]
