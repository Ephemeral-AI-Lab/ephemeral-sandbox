"""Layer-stack publish transaction boundary."""

from __future__ import annotations

import threading
from collections.abc import Sequence
from contextlib import AbstractContextManager
from pathlib import Path
from types import TracebackType
from typing import TYPE_CHECKING

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.manifest import Manifest, read_manifest
from sandbox._shared.clock import monotonic_now

if TYPE_CHECKING:
    from sandbox.layer_stack.publisher import LayerPublisher
    from sandbox.layer_stack.storage_lock import StorageWriterLockLease


class LayerStackTransaction:
    """Process-local active-manifest transaction shell."""

    def __init__(
        self,
        *,
        lock: threading.RLock,
        manifest_path: Path,
        publisher: LayerPublisher,
        storage_writer_lock: StorageWriterLockLease,
    ) -> None:
        self._lock = lock
        self._manifest_path = manifest_path
        self._publisher = publisher
        self._storage_writer_lock = storage_writer_lock
        self._storage_guard: AbstractContextManager[object] | None = None
        self._manifest: Manifest | None = None
        self._entered = False
        self._lock_acquired_at: float | None = None
        self._lock_held_s = 0.0
        self._lock_wait_s = 0.0

    def __enter__(self) -> LayerStackTransaction:
        wait_start = monotonic_now()
        storage_guard = self._storage_writer_lock.exclusive()
        storage_guard.__enter__()
        self._storage_guard = storage_guard
        try:
            self._lock.acquire()
        except BaseException:
            self._release_storage_guard(None, None, None)
            raise
        acquired_at = monotonic_now()
        self._lock_wait_s = acquired_at - wait_start
        self._lock_acquired_at = acquired_at
        self._entered = True
        self._manifest = read_manifest(self._manifest_path)
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self._entered = False
        self._manifest = None
        if self._lock_acquired_at is not None:
            self._lock_held_s = monotonic_now() - self._lock_acquired_at
            self._lock_acquired_at = None
        self._lock.release()
        self._release_storage_guard(exc_type, exc, traceback)

    def snapshot(self) -> Manifest:
        return self._require_manifest()

    def publish_layer(
        self,
        changes: Sequence[LayerChange],
        *,
        source_root: str | Path | None = None,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        current = self._require_manifest()
        new_manifest = self._publisher.publish_layer(
            tuple(changes),
            expected_manifest=current,
            source_root=source_root,
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
            return monotonic_now() - self._lock_acquired_at
        return self._lock_held_s

    def _require_manifest(self) -> Manifest:
        if not self._entered or self._manifest is None:
            raise RuntimeError("layer-stack transaction is not active")
        return self._manifest

    def _release_storage_guard(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        guard = self._storage_guard
        self._storage_guard = None
        if guard is not None:
            guard.__exit__(exc_type, exc, traceback)


__all__ = ["LayerStackTransaction"]
