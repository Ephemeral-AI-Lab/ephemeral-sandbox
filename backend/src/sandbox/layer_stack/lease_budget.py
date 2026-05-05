"""Lease-pressure decisions for sandbox layer stacks."""

from __future__ import annotations

import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from typing import Literal

from sandbox.layer_stack.manifest import LayerRef


BudgetDecisionKind = Literal[
    "allow",
    "warn",
    "kill_lease",
    "backpressure_commits",
    "evict_session",
]


@dataclass(frozen=True)
class LeaseSnapshot:
    lease_id: str
    owner_id: str
    manifest_version: int
    pinned_layers: tuple[LayerRef, ...]
    pinned_bytes: int
    acquired_at: float

    def __post_init__(self) -> None:
        object.__setattr__(self, "pinned_layers", tuple(self.pinned_layers))
        if self.pinned_bytes < 0:
            raise ValueError("pinned_bytes must be non-negative")


@dataclass(frozen=True)
class BudgetDecision:
    kind: BudgetDecisionKind
    reason: str
    lease_id: str | None = None


class LeaseBudgetWorker:
    """Evaluates lease age, pinned bytes, and active-depth pressure."""

    DEFAULT_KILL_LEASE_AGE_SECONDS: float = 1800.0  # 30 minutes

    def __init__(
        self,
        *,
        max_active_depth: int | None = None,
        max_pinned_bytes: int | None = None,
        warn_lease_age_seconds: float | None = None,
        kill_lease_age_seconds: float | None = DEFAULT_KILL_LEASE_AGE_SECONDS,
        evict_session_pinned_bytes: int | None = None,
        clock: Callable[[], float] | None = None,
    ) -> None:
        _validate_non_negative_int("max_active_depth", max_active_depth)
        _validate_non_negative_int("max_pinned_bytes", max_pinned_bytes)
        _validate_positive_float("warn_lease_age_seconds", warn_lease_age_seconds)
        _validate_positive_float("kill_lease_age_seconds", kill_lease_age_seconds)
        _validate_non_negative_int(
            "evict_session_pinned_bytes",
            evict_session_pinned_bytes,
        )
        self._max_active_depth = max_active_depth
        self._max_pinned_bytes = max_pinned_bytes
        self._warn_lease_age_seconds = warn_lease_age_seconds
        self._kill_lease_age_seconds = kill_lease_age_seconds
        self._evict_session_pinned_bytes = evict_session_pinned_bytes
        self._clock = clock or time.time

    def evaluate(
        self,
        *,
        active_depth: int,
        snapshots: Sequence[LeaseSnapshot],
    ) -> BudgetDecision:
        if active_depth < 0:
            raise ValueError("active_depth must be non-negative")

        ordered_snapshots = tuple(sorted(snapshots, key=lambda snapshot: snapshot.acquired_at))
        total_pinned_bytes = sum(snapshot.pinned_bytes for snapshot in ordered_snapshots)

        if self._max_active_depth is not None and active_depth >= self._max_active_depth:
            return BudgetDecision(
                kind="backpressure_commits",
                reason=(
                    f"active manifest depth {active_depth} reached limit {self._max_active_depth}"
                ),
            )

        if self._max_pinned_bytes is not None and total_pinned_bytes >= self._max_pinned_bytes:
            return BudgetDecision(
                kind="backpressure_commits",
                reason=(
                    f"snapshot leases pin {total_pinned_bytes} bytes, "
                    f"limit {self._max_pinned_bytes}"
                ),
            )

        eviction_target = self._first_snapshot_over_pinned_limit(ordered_snapshots)
        if eviction_target is not None:
            return BudgetDecision(
                kind="evict_session",
                reason=(
                    f"lease {eviction_target.lease_id} pins "
                    f"{eviction_target.pinned_bytes} bytes, "
                    f"limit {self._evict_session_pinned_bytes}"
                ),
                lease_id=eviction_target.lease_id,
            )

        now = self._clock()
        kill_target = self._first_snapshot_over_age(
            ordered_snapshots,
            now=now,
            max_age_seconds=self._kill_lease_age_seconds,
        )
        if kill_target is not None:
            return BudgetDecision(
                kind="kill_lease",
                reason=(
                    f"lease {kill_target.lease_id} age "
                    f"{now - kill_target.acquired_at:.1f}s reached "
                    f"limit {self._kill_lease_age_seconds:.1f}s"
                ),
                lease_id=kill_target.lease_id,
            )

        warn_target = self._first_snapshot_over_age(
            ordered_snapshots,
            now=now,
            max_age_seconds=self._warn_lease_age_seconds,
        )
        if warn_target is not None:
            return BudgetDecision(
                kind="warn",
                reason=(
                    f"lease {warn_target.lease_id} age "
                    f"{now - warn_target.acquired_at:.1f}s reached "
                    f"warning threshold {self._warn_lease_age_seconds:.1f}s"
                ),
                lease_id=warn_target.lease_id,
            )

        return BudgetDecision(kind="allow", reason="lease budget allows commits")

    def _first_snapshot_over_pinned_limit(
        self,
        snapshots: Sequence[LeaseSnapshot],
    ) -> LeaseSnapshot | None:
        if self._evict_session_pinned_bytes is None:
            return None
        for snapshot in snapshots:
            if snapshot.pinned_bytes >= self._evict_session_pinned_bytes:
                return snapshot
        return None

    def _first_snapshot_over_age(
        self,
        snapshots: Sequence[LeaseSnapshot],
        *,
        now: float,
        max_age_seconds: float | None,
    ) -> LeaseSnapshot | None:
        if max_age_seconds is None:
            return None
        for snapshot in snapshots:
            if now - snapshot.acquired_at >= max_age_seconds:
                return snapshot
        return None


def _validate_non_negative_int(name: str, value: int | None) -> None:
    if value is not None and value < 0:
        raise ValueError(f"{name} must be non-negative")


def _validate_positive_float(name: str, value: float | None) -> None:
    if value is not None and value <= 0:
        raise ValueError(f"{name} must be positive")
