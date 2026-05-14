"""Post-publish OCC maintenance policies."""

from __future__ import annotations

import threading
from dataclasses import dataclass, field
from typing import Protocol, runtime_checkable

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.types import ChangesetResult
from sandbox.occ.ports import SnapshotReader
from sandbox.timing import monotonic_now


class MaintenancePolicy(Protocol):
    """Post-publish maintenance hook for OCC service commits."""

    def after_publish_sync(self, result: ChangesetResult) -> dict[str, float]: ...


@runtime_checkable
class SquashPort(Protocol):
    """Layer-stack maintenance capability consumed by auto-squash."""

    def squash(self, *, max_depth: int) -> Manifest | None: ...


@dataclass
class _CoalescedSquashState:
    lock: threading.Lock = field(default_factory=threading.Lock)
    state_lock: threading.Lock = field(default_factory=threading.Lock)
    pending_recheck: bool = False


@dataclass(frozen=True)
class NoopMaintenancePolicy:
    """Maintenance policy for callers that do not want post-publish work."""

    def after_publish_sync(self, result: ChangesetResult) -> dict[str, float]:
        del result
        return {}


class AutoSquashMaintenancePolicy:
    """Coalesced synchronous layer-stack squash after successful publishes."""

    def __init__(
        self,
        *,
        snapshot_reader: SnapshotReader,
        squasher: SquashPort,
        max_depth: int,
    ) -> None:
        self._snapshot_reader = snapshot_reader
        self._squasher = squasher
        self._max_depth = int(max_depth)
        self._state = _CoalescedSquashState()

    def after_publish_sync(self, result: ChangesetResult) -> dict[str, float]:
        if result.published_manifest_version is None:
            return {}
        active = self._snapshot_reader.read_active_manifest()
        if active.depth <= self._max_depth:
            return {}

        state = self._state
        if not state.lock.acquire(blocking=False):
            with state.state_lock:
                state.pending_recheck = True
            return {
                "layer_stack.auto_squash.skipped_in_flight": 1.0,
                "layer_stack.auto_squash.max_depth": float(self._max_depth),
                "layer_stack.auto_squash.depth_before": float(active.depth),
            }

        try:
            timings = self._run_squash_for_active(active)
            with state.state_lock:
                pending_recheck = state.pending_recheck
                state.pending_recheck = False
            if not pending_recheck:
                return timings

            active = self._snapshot_reader.read_active_manifest()
            if active.depth <= self._max_depth:
                return timings
            recheck_timings = self._run_squash_for_active(active)
            recheck_timings["layer_stack.auto_squash.recheck_triggered"] = 1.0
            return _merge_auto_squash_timings(timings, recheck_timings)
        finally:
            state.lock.release()

    def _run_squash_for_active(self, active: Manifest) -> dict[str, float]:
        squash_start = monotonic_now()
        squashed = self._squasher.squash(max_depth=self._max_depth)
        elapsed = monotonic_now() - squash_start
        timings = {
            "layer_stack.auto_squash.total_s": elapsed,
            "layer_stack.auto_squash.max_depth": float(self._max_depth),
            "layer_stack.auto_squash.depth_before": float(active.depth),
        }
        if squashed is None:
            timings["layer_stack.auto_squash.raced"] = 1.0
            return timings
        timings["layer_stack.auto_squash.depth_after"] = float(squashed.depth)
        timings["layer_stack.auto_squash.manifest_version"] = float(squashed.version)
        return timings


def _merge_auto_squash_timings(
    first: dict[str, float],
    second: dict[str, float],
) -> dict[str, float]:
    if not first:
        return dict(second)
    if not second:
        return dict(first)
    merged = {**first, **second}
    if (
        "layer_stack.auto_squash.total_s" in first
        or "layer_stack.auto_squash.total_s" in second
    ):
        merged["layer_stack.auto_squash.total_s"] = first.get(
            "layer_stack.auto_squash.total_s",
            0.0,
        ) + second.get("layer_stack.auto_squash.total_s", 0.0)
    if "layer_stack.auto_squash.depth_before" in first:
        merged["layer_stack.auto_squash.depth_before"] = first[
            "layer_stack.auto_squash.depth_before"
        ]
    return merged


__all__ = [
    "AutoSquashMaintenancePolicy",
    "MaintenancePolicy",
    "NoopMaintenancePolicy",
    "SquashPort",
]
