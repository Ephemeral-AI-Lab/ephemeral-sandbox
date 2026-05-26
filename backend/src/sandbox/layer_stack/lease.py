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
    """Tracks active snapshot leases and the layers they retain on disk."""

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

    def leased_layers(self) -> tuple[LayerRef, ...]:
        """Return every layer retained on disk by at least one active lease.

        The lease's manifest references each of these layer directories for
        reads, so GC must keep them on disk until the lease releases. This is
        the full retention set; see :meth:`lease_head_layers` for the smaller
        set of layers that act as squash barriers.
        """
        with self._lock:
            return tuple(sorted(self._refcounts))

    def lease_head_layers(self) -> tuple[LayerRef, ...]:
        """Return the newest layer of each active lease's manifest.

        Squash uses these as barriers: each lease's head is a snapshot cut
        point that must remain visible in the active manifest. Layers below
        the head are foldable; the lease itself keeps reading through its
        own frozen manifest via :meth:`leased_layers` GC retention.
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
