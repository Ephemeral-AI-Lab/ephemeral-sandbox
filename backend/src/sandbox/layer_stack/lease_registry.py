"""Exact layer-ref lease registry for frozen layer-stack snapshots."""

from __future__ import annotations

import threading
import time
import uuid
from collections import Counter
from collections.abc import Callable, Iterable
from dataclasses import dataclass

from sandbox.layer_stack.manifest import LayerRef, Manifest


@dataclass(frozen=True)
class Lease:
    lease_id: str
    manifest: Manifest
    owner_id: str
    acquired_at: float


class LeaseRegistry:
    """Tracks active snapshot leases and exact pinned layer refs."""

    def __init__(
        self,
        *,
        id_factory: Callable[[], str] | None = None,
        clock: Callable[[], float] | None = None,
    ) -> None:
        self._id_factory = id_factory or (lambda: uuid.uuid4().hex)
        self._clock = clock or time.time
        self._lock = threading.RLock()
        self._leases: dict[str, Lease] = {}
        self._refcounts: Counter[LayerRef] = Counter()

    def acquire(self, manifest: Manifest, owner_id: str) -> Lease:
        if not owner_id:
            raise ValueError("owner_id must not be empty")
        with self._lock:
            lease = Lease(
                lease_id=self._id_factory(),
                manifest=manifest,
                owner_id=owner_id,
                acquired_at=self._clock(),
            )
            self._leases[lease.lease_id] = lease
            self._refcounts.update(manifest.layers)
            return lease

    def release(self, lease_id: str) -> Lease | None:
        with self._lock:
            return self._release_locked(lease_id)

    def expire_older_than(
        self,
        max_age_seconds: float,
        *,
        now: float | None = None,
    ) -> tuple[Lease, ...]:
        if max_age_seconds < 0:
            raise ValueError("max_age_seconds must be non-negative")
        cutoff = (self._clock() if now is None else now) - max_age_seconds
        with self._lock:
            expired_ids = (
                lease.lease_id
                for lease in self._ordered_leases_locked()
                if lease.acquired_at <= cutoff
            )
            return self._release_many_locked(expired_ids)

    def sweep_dead_owners(self, live_owner_ids: Iterable[str]) -> tuple[Lease, ...]:
        live = set(live_owner_ids)
        with self._lock:
            dead_ids = (
                lease.lease_id
                for lease in self._ordered_leases_locked()
                if lease.owner_id not in live
            )
            return self._release_many_locked(dead_ids)

    def refcount(self, layer: LayerRef) -> int:
        with self._lock:
            return self._refcounts.get(layer, 0)

    def pinned_layers(self) -> tuple[LayerRef, ...]:
        with self._lock:
            return tuple(sorted(self._refcounts))

    def active_leases(self) -> tuple[Lease, ...]:
        with self._lock:
            return self._ordered_leases_locked()

    def _ordered_leases_locked(self) -> tuple[Lease, ...]:
        return tuple(sorted(self._leases.values(), key=lambda lease: lease.acquired_at))

    def _release_many_locked(self, lease_ids: Iterable[str]) -> tuple[Lease, ...]:
        released: list[Lease] = []
        for lease_id in tuple(lease_ids):
            lease = self._release_locked(lease_id)
            if lease is not None:
                released.append(lease)
        return tuple(released)

    def _release_locked(self, lease_id: str) -> Lease | None:
        lease = self._leases.pop(lease_id, None)
        if lease is None:
            return None
        for layer in lease.manifest.layers:
            self._refcounts[layer] -= 1
            if self._refcounts[layer] <= 0:
                del self._refcounts[layer]
        return lease
