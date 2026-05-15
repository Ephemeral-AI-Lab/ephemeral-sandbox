"""Layer-stack publish transaction boundary."""

from __future__ import annotations

import threading
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from types import TracebackType
from typing import TYPE_CHECKING

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.manifest import Manifest
from sandbox._shared.clock import monotonic_now

if TYPE_CHECKING:
    from sandbox.layer_stack.publisher import LayerPublisher
    from sandbox.layer_stack.manifest import FileManifestStore


@dataclass(frozen=True)
class LayerStackTransactionHandle:
    lock: threading.RLock
    manifest_store: FileManifestStore
    publisher: LayerPublisher


class LayerStackTransaction:
    """Process-local active-manifest transaction shell."""

    def __init__(self, handle: LayerStackTransactionHandle) -> None:
        self._handle = handle
        self._manifest: Manifest | None = None
        self._entered = False
        self._lock_acquired_at: float | None = None
        self._lock_held_s = 0.0
        self._lock_wait_s = 0.0

    def __enter__(self) -> LayerStackTransaction:
        wait_start = monotonic_now()
        self._handle.lock.acquire()
        acquired_at = monotonic_now()
        self._lock_wait_s = acquired_at - wait_start
        self._lock_acquired_at = acquired_at
        self._entered = True
        self._manifest = self._handle.manifest_store.read()
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        del exc_type, exc, traceback
        self._entered = False
        self._manifest = None
        if self._lock_acquired_at is not None:
            self._lock_held_s = monotonic_now() - self._lock_acquired_at
            self._lock_acquired_at = None
        self._handle.lock.release()

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
        new_manifest = self._handle.publisher.publish_layer(
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


__all__ = [
    "LayerStackTransaction",
    "LayerStackTransactionHandle",
]
