"""Sole owner of per-pipeline lease-destroy race protection.

Owns the lease-keyed lock dict and the released-lease-id set. Both
``EphemeralPipeline`` and any future pipeline that needs idempotent lease
release compose one instance — single source of truth for the destroy/release
race semantics.

Iws does not compose this class today: its per-call execution path leases
nothing (the lease lives on the persistent ``IsolatedWorkspaceHandle`` and is
released exactly once inside ``_teardown``). Adding a ``LeaseGuard`` to iws
would be manufactured symmetry; the class is here for genuine concurrent-OCC
clients (eph today, future pipelines tomorrow).
"""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from typing import Protocol


class _LeasedHandle(Protocol):
    """Minimum surface ``destroy`` needs from a handle."""

    lease_id: str
    _destroyed: bool


class LeaseGuard:
    """Lease-id-keyed lock + released-set composed by pipelines that
    publish through OCC.

    Two responsibilities, one place:

    * ``destroy(handle, destroy_fn)`` runs ``destroy_fn`` under the lease's
      lock, short-circuits if the handle is already destroyed or its lease
      has already been released, and pops the per-lease lock at the end.
    * ``mark_released(lease_id)`` records that the lease has been released
      via a path other than ``destroy`` (e.g. eph's ``_release_lease``),
      preserving the idempotent re-release guarantee.
    """

    def __init__(self) -> None:
        self._lease_locks: dict[str, asyncio.Lock] = {}
        self._released_lease_ids: set[str] = set()

    def _lock_for(self, lease_id: str) -> asyncio.Lock:
        lock = self._lease_locks.get(lease_id)
        if lock is None:
            lock = self._lease_locks[lease_id] = asyncio.Lock()
        return lock

    async def destroy(
        self,
        handle: _LeasedHandle,
        destroy_fn: Callable[[_LeasedHandle], Awaitable[None]],
    ) -> None:
        async with self._lock_for(handle.lease_id):
            try:
                if handle._destroyed:
                    return
                if handle.lease_id and handle.lease_id in self._released_lease_ids:
                    handle._destroyed = True
                    return
                if handle.lease_id:
                    self._released_lease_ids.add(handle.lease_id)
                await destroy_fn(handle)
            finally:
                self._lease_locks.pop(handle.lease_id, None)

    def mark_released(self, lease_id: str) -> bool:
        """Atomically record that ``lease_id`` has been released.

        Returns ``True`` when the lease was newly marked, ``False`` when it
        was already in the released set. Callers route the actual
        ``layer_stack.release_lease`` call on the ``True`` branch so the
        idempotent re-release guarantee is preserved without duplicating
        the set membership check at the caller site.
        """
        if not lease_id:
            return False
        if lease_id in self._released_lease_ids:
            return False
        self._released_lease_ids.add(lease_id)
        return True


__all__ = ["LeaseGuard"]
