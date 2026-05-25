"""Exact layer-ref lease registry for frozen layer-stack snapshots."""

from __future__ import annotations

import threading
import uuid
from collections import Counter
from collections.abc import Callable
from dataclasses import dataclass

from sandbox.layer_stack.manifest import LayerRef, Manifest


@dataclass(frozen=True)
class WorkspaceLease:
    lease_id: str
    manifest: Manifest


class LeaseRegistry:
    """Tracks active snapshot leases and exact pinned layer refs."""

    def __init__(
        self,
        *,
        id_factory: Callable[[], str] | None = None,
    ) -> None:
        self._id_factory = id_factory or (lambda: uuid.uuid4().hex)
        self._lock = threading.RLock()
        self._leases: dict[str, WorkspaceLease] = {}
        self._refcounts: Counter[LayerRef] = Counter()

    def acquire(
        self,
        manifest: Manifest,
        owner_request_id: str,
    ) -> WorkspaceLease:
        if not owner_request_id:
            raise ValueError("owner_request_id must not be empty")
        with self._lock:
            lease = WorkspaceLease(
                lease_id=self._id_factory(),
                manifest=manifest,
            )
            self._leases[lease.lease_id] = lease
            self._refcounts.update(manifest.layers)
            return lease

    def release(self, lease_id: str) -> WorkspaceLease | None:
        with self._lock:
            lease = self._leases.pop(lease_id, None)
            if lease is None:
                return None
            self._refcounts -= Counter(lease.manifest.layers)
            return lease

    def pinned_layers(self) -> tuple[LayerRef, ...]:
        with self._lock:
            return tuple(sorted(self._refcounts))

    def squash_barrier_layers(self) -> tuple[LayerRef, ...]:
        """Return leased snapshot boundary layers that active squash must preserve.

        Snapshot leases pin every layer for GC, but treating every pinned layer
        as an active-manifest squash barrier prevents any reduction for a
        leased deep snapshot. The newest layer of each leased manifest is the
        boundary that keeps active squash from crossing that snapshot cut; the
        remaining leased layers stay GC-pinned until release.
        """
        with self._lock:
            return tuple(
                sorted(
                    {
                        lease.manifest.layers[0]
                        for lease in self._leases.values()
                        if lease.manifest.layers
                    }
                )
            )

    def active_count(self) -> int:
        with self._lock:
            return len(self._leases)


__all__ = ["LeaseRegistry", "WorkspaceLease"]
