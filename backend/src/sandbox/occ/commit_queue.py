"""Global commit queue for prepared OCC commits."""

from __future__ import annotations

import asyncio
import concurrent.futures
import queue
import threading
import time
from dataclasses import dataclass

from sandbox.layer_stack.manifest import ManifestConflictError
from sandbox.occ.changeset import ChangeSource, PreparedChangeset, RouteDecision
from sandbox.occ.changeset import ChangesetResult, FileResult, FileStatus
from sandbox.occ.commit_transaction import CommitTransaction
from sandbox._shared.timing_keys import TimingKey
from sandbox._shared.clock import monotonic_now

_RESULT_READY_AT = TimingKey.SERIAL_RESULT_READY_AT


MAX_OCC_CAS_RETRIES: int = 3
"""Phase 05 — bounded CAS-mismatch retry budget.

If the layer-stack publisher returns a manifest CAS mismatch
(:class:`ManifestConflictError`) during ``revalidate_and_publish``, the
serial merger re-runs validation up to ``MAX_OCC_CAS_RETRIES`` times. On
exhaustion, the call surfaces a conflict ``ChangesetResult`` (every path
marked ``ABORTED_VERSION``) instead of looping indefinitely.

In the current single-process architecture the per-root publisher lock
makes mid-transaction CAS races structurally impossible, so the retry
loop is a defensive bound — every commit succeeds on the first attempt.
The constant exists so multi-process Phase 06+ topologies inherit a
named, testable limit.
"""


@dataclass(frozen=True)
class _WorkItem:
    prepared: PreparedChangeset
    future: concurrent.futures.Future[ChangesetResult]
    enqueued_at: float


class _StopItem:
    __slots__ = ()


_STOP = _StopItem()
_QueueItem = _WorkItem | _StopItem


class CommitQueue:
    """Serialize OCC publish while batching disjoint prepared changesets."""

    def __init__(
        self,
        transaction: CommitTransaction,
        *,
        max_batch_size: int = 64,
        batch_window_s: float = 0.002,
        max_cas_retries: int = MAX_OCC_CAS_RETRIES,
    ) -> None:
        if max_cas_retries < 1:
            raise ValueError("max_cas_retries must be >= 1")
        self._transaction = transaction
        self._max_batch_size = max(1, int(max_batch_size))
        self._batch_window_s = max(0.0, float(batch_window_s))
        self._max_cas_retries = int(max_cas_retries)
        self._queue: queue.Queue[_QueueItem] = queue.Queue()
        self._thread: threading.Thread | None = None
        self._state_lock = threading.Lock()
        self._closed = False

    def start(self) -> None:
        """Start the background commit worker."""
        with self._state_lock:
            if self._closed:
                raise RuntimeError("OCC commit queue is closed")
            if self._thread is not None and self._thread.is_alive():
                return
            self._thread = threading.Thread(
                target=self._run,
                name="occ-commit-queue",
                daemon=True,
            )
            self._thread.start()

    def close(self, *, timeout: float | None = 5.0) -> None:
        """Stop the background commit worker after pending queued work drains."""
        with self._state_lock:
            if self._closed:
                return
            self._closed = True
            thread = self._thread
            if thread is None:
                return
            self._queue.put(_STOP)
        thread.join(timeout=timeout)

    def submit(
        self,
        prepared: PreparedChangeset,
    ) -> concurrent.futures.Future[ChangesetResult]:
        future: concurrent.futures.Future[ChangesetResult] = concurrent.futures.Future()
        with self._state_lock:
            if self._closed:
                raise RuntimeError("OCC commit queue is closed")
            if self._thread is None or not self._thread.is_alive():
                raise RuntimeError("OCC commit queue has not been started")
            self._queue.put(
                _WorkItem(
                    prepared=prepared,
                    future=future,
                    enqueued_at=monotonic_now(),
                )
            )
        return future

    async def apply(self, prepared: PreparedChangeset) -> ChangesetResult:
        return await asyncio.wrap_future(self.submit(prepared))

    def apply_sync(self, prepared: PreparedChangeset) -> ChangesetResult:
        return self.submit(prepared).result()

    def _run(self) -> None:
        while True:
            first = self._queue.get()
            if isinstance(first, _StopItem):
                return
            items = [first]
            stop_seen = False
            # WR-04: drain the queue non-blockingly first. Only pay the
            # batch-window latency when the drain emptied the queue AND
            # we still have headroom; otherwise the sleep is dead
            # wall-clock on the single-commit hot path.
            while len(items) < self._max_batch_size:
                try:
                    item = self._queue.get_nowait()
                except queue.Empty:
                    break
                if isinstance(item, _StopItem):
                    stop_seen = True
                    break
                items.append(item)
            if not stop_seen and self._batch_window_s > 0 and len(items) < self._max_batch_size:
                time.sleep(self._batch_window_s)
                while len(items) < self._max_batch_size:
                    try:
                        item = self._queue.get_nowait()
                    except queue.Empty:
                        break
                    if isinstance(item, _StopItem):
                        stop_seen = True
                        break
                    items.append(item)

            pending = [item for item in items if not item.future.cancelled()]
            for batch in _disjoint_batches(pending):
                self._commit_batch(batch)
            if stop_seen:
                return

    def _commit_batch(self, batch: list[_WorkItem]) -> None:
        if not batch:
            return
        commit_start = monotonic_now()
        combined = _combine_prepared([item.prepared for item in batch])
        attempts = 0
        try:
            while True:
                try:
                    result = self._transaction.revalidate_and_publish(combined)
                    break
                except ManifestConflictError as exc:
                    attempts += 1
                    if attempts >= self._max_cas_retries:
                        result = _cas_exhaustion_result(
                            combined,
                            exc,
                            max_cas_retries=self._max_cas_retries,
                        )
                        break
            commit_elapsed = monotonic_now() - commit_start
            ready_at = monotonic_now()
            for item in batch:
                if item.future.cancelled():
                    continue
                paths = _path_set(item.prepared)
                files = tuple(file for file in result.files if file.path in paths)
                item.future.set_result(
                    ChangesetResult(
                        files=files,
                        timings={
                            **item.prepared.timings,
                            **result.timings,
                            TimingKey.SERIAL_QUEUE_WAIT: commit_start - item.enqueued_at,
                            TimingKey.SERIAL_BATCH_SIZE: float(len(batch)),
                            TimingKey.SERIAL_COMMIT: commit_elapsed,
                            TimingKey.SERIAL_CAS_ATTEMPTS: float(attempts + 1),
                            _RESULT_READY_AT: ready_at,
                        },
                        published_manifest_version=result.published_manifest_version,
                    )
                )
        except BaseException as exc:
            for item in batch:
                if not item.future.done():
                    item.future.set_exception(exc)
            if not isinstance(exc, Exception):
                raise


def _disjoint_batches(items: list[_WorkItem]) -> list[list[_WorkItem]]:
    batches: list[list[_WorkItem]] = []
    pending = list(items)
    while pending:
        used_paths: set[str] = set()
        batch: list[_WorkItem] = []
        rest: list[_WorkItem] = []
        for item in pending:
            paths = _path_set(item.prepared)
            if (
                item.prepared.atomic
                or _contains_overlay_capture(item.prepared)
                or used_paths.intersection(paths)
            ):
                rest.append(item)
                continue
            batch.append(item)
            used_paths.update(paths)
        if batch:
            batches.append(batch)
            pending = rest
            continue
        batches.append([pending.pop(0)])
    return batches


def _combine_prepared(items: list[PreparedChangeset]) -> PreparedChangeset:
    first = items[0]
    if len(items) > 1 and any(prepared.atomic for prepared in items):
        raise AssertionError("atomic prepared changesets must not be batched")
    return PreparedChangeset(
        snapshot=first.snapshot,
        path_groups=tuple(group for prepared in items for group in prepared.path_groups),
        atomic=first.atomic,
        timings=_merge_timings(items),
    )


def _path_set(prepared: PreparedChangeset) -> set[str]:
    return {group.path for group in prepared.path_groups}


def _contains_overlay_capture(prepared: PreparedChangeset) -> bool:
    return any(
        change.source is ChangeSource.OVERLAY_CAPTURE
        for group in prepared.path_groups
        for change in group.changes
    )


def _cas_exhaustion_result(
    prepared: PreparedChangeset,
    exc: ManifestConflictError,
    *,
    max_cas_retries: int,
) -> ChangesetResult:
    """Convert a CAS-retry-exhausted failure into a per-path conflict result."""
    message = f"CAS mismatch retry budget exhausted after {max_cas_retries} attempts: {exc}"
    files: list[FileResult] = []
    for group in prepared.path_groups:
        if group.route is RouteDecision.DROP:
            files.append(
                FileResult(
                    path=group.path,
                    status=FileStatus.DROPPED,
                    message=group.message or "change dropped",
                )
            )
            continue
        if group.route is RouteDecision.REJECT:
            files.append(
                FileResult(
                    path=group.path,
                    status=FileStatus.REJECTED,
                    message=group.message or "change rejected",
                )
            )
            continue
        files.append(
            FileResult(
                path=group.path,
                status=FileStatus.ABORTED_VERSION,
                message=message,
            )
        )
    return ChangesetResult(
        files=tuple(files),
        timings={TimingKey.SERIAL_CAS_EXHAUSTED: 1.0},
        published_manifest_version=None,
    )


def _merge_timings(items: list[PreparedChangeset]) -> dict[str, float]:
    timings: dict[str, float] = {}
    for prepared in items:
        for key, value in prepared.timings.items():
            timings[key] = timings.get(key, 0.0) + float(value)
    return timings


__all__ = ["MAX_OCC_CAS_RETRIES", "CommitQueue"]
