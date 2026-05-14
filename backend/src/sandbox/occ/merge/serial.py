"""Global serial merger for prepared OCC commits."""

from __future__ import annotations

import asyncio
import concurrent.futures
import queue
import threading
import time
from dataclasses import dataclass

from sandbox.layer_stack.manifest import ManifestConflictError
from sandbox.occ.changeset.prepared import PreparedChangeset, RouteDecision
from sandbox.occ.changeset.types import ChangesetResult, FileResult, FileStatus
from sandbox.occ.commit_transaction import OccCommitTransaction
from sandbox.timing import monotonic_now

_RESULT_READY_AT = "_occ.serial.result_ready_at_s"


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
class RetryPolicy:
    """Bounded CAS retry policy for serial OCC commits."""

    max_cas_retries: int = MAX_OCC_CAS_RETRIES

    def __post_init__(self) -> None:
        if self.max_cas_retries < 1:
            raise ValueError("max_cas_retries must be >= 1")


@dataclass(frozen=True)
class _WorkItem:
    prepared: PreparedChangeset
    future: concurrent.futures.Future[ChangesetResult]
    enqueued_at: float


@dataclass(frozen=True)
class _StopItem:
    pass


_STOP = _StopItem()
_QueueItem = _WorkItem | _StopItem


class OccSerialMerger:
    """Serialize OCC publish while batching disjoint prepared changesets."""

    def __init__(
        self,
        transaction: OccCommitTransaction,
        *,
        max_batch_size: int = 64,
        batch_window_s: float = 0.002,
        retry_policy: RetryPolicy = RetryPolicy(),
    ) -> None:
        self._transaction = transaction
        self._max_batch_size = max(1, int(max_batch_size))
        self._batch_window_s = max(0.0, float(batch_window_s))
        self._retry_policy = retry_policy
        self._queue: queue.Queue[_QueueItem] = queue.Queue()
        self._thread: threading.Thread | None = None
        self._closed = False

    def start(self) -> None:
        """Start the background commit worker."""
        if self._closed:
            raise RuntimeError("OCC serial merger is closed")
        if self._thread is not None and self._thread.is_alive():
            return
        self._thread = threading.Thread(
            target=self._run,
            name="occ-serial-merger",
            daemon=True,
        )
        self._thread.start()

    def close(self, *, timeout: float | None = 5.0) -> None:
        """Stop the background commit worker after pending queued work drains."""
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
        if self._closed:
            raise RuntimeError("OCC serial merger is closed")
        if self._thread is None or not self._thread.is_alive():
            raise RuntimeError("OCC serial merger has not been started")
        future: concurrent.futures.Future[ChangesetResult] = (
            concurrent.futures.Future()
        )
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
                    self._queue.put(item)
                    break
                items.append(item)
            if (
                self._batch_window_s > 0
                and len(items) < self._max_batch_size
            ):
                time.sleep(self._batch_window_s)
                while len(items) < self._max_batch_size:
                    try:
                        item = self._queue.get_nowait()
                    except queue.Empty:
                        break
                    if isinstance(item, _StopItem):
                        self._queue.put(item)
                        break
                    items.append(item)

            pending = [item for item in items if not item.future.cancelled()]
            for batch in _disjoint_batches(pending):
                self._commit_batch(batch)

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
                    if attempts >= self._retry_policy.max_cas_retries:
                        result = _cas_exhaustion_result(
                            combined,
                            exc,
                            retry_policy=self._retry_policy,
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
                            "occ.serial.queue_wait_s": commit_start
                            - item.enqueued_at,
                            "occ.serial.batch_size": float(len(batch)),
                            "occ.serial.commit_s": commit_elapsed,
                            "occ.serial.cas_attempts": float(attempts + 1),
                            _RESULT_READY_AT: ready_at,
                        },
                        published_manifest_version=result.published_manifest_version,
                    )
                )
        except BaseException as exc:
            for item in batch:
                if not item.future.cancelled():
                    item.future.set_exception(exc)


def _disjoint_batches(items: list[_WorkItem]) -> list[list[_WorkItem]]:
    batches: list[list[_WorkItem]] = []
    pending = list(items)
    while pending:
        used_paths: set[str] = set()
        batch: list[_WorkItem] = []
        rest: list[_WorkItem] = []
        for item in pending:
            paths = _path_set(item.prepared)
            if item.prepared.atomic or used_paths.intersection(paths):
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
        path_groups=tuple(
            group for prepared in items for group in prepared.path_groups
        ),
        atomic=first.atomic,
        timings=_merge_timings(items),
    )


def _path_set(prepared: PreparedChangeset) -> set[str]:
    return {group.path for group in prepared.path_groups}


def _cas_exhaustion_result(
    prepared: PreparedChangeset,
    exc: ManifestConflictError,
    *,
    retry_policy: RetryPolicy,
) -> ChangesetResult:
    """Convert a CAS-retry-exhausted failure into a per-path conflict result."""
    message = (
        f"CAS mismatch retry budget exhausted after {retry_policy.max_cas_retries} "
        f"attempts: {exc}"
    )
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
        timings={"occ.serial.cas_exhausted": 1.0},
        published_manifest_version=None,
    )


def _merge_timings(items: list[PreparedChangeset]) -> dict[str, float]:
    timings: dict[str, float] = {}
    for prepared in items:
        for key, value in prepared.timings.items():
            timings[key] = timings.get(key, 0.0) + float(value)
    return timings


__all__ = ["MAX_OCC_CAS_RETRIES", "OccSerialMerger", "RetryPolicy"]
