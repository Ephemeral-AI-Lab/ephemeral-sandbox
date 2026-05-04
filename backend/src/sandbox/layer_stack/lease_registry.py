"""Exact layer-ref lease registry for frozen layer-stack snapshots."""

from __future__ import annotations

import threading
import time
import uuid
from collections import Counter
from collections.abc import Callable
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
            lease = self._leases.pop(lease_id, None)
            if lease is None:
                return None
            for layer in lease.manifest.layers:
                self._refcounts[layer] -= 1
                if self._refcounts[layer] <= 0:
                    del self._refcounts[layer]
            return lease

    def refcount(self, layer: LayerRef) -> int:
        with self._lock:
            return self._refcounts.get(layer, 0)

    def pinned_layers(self) -> tuple[LayerRef, ...]:
        with self._lock:
            return tuple(sorted(self._refcounts))

    def active_leases(self) -> tuple[Lease, ...]:
        with self._lock:
            return tuple(sorted(self._leases.values(), key=lambda lease: lease.acquired_at))
