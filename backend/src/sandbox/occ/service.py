"""OCC changeset preparation and commit service."""

from __future__ import annotations

import time
import asyncio
import os
import threading
from collections.abc import Sequence
from dataclasses import dataclass, field, replace
from typing import Literal, cast

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.commit_transaction import OccCommitTransaction
from sandbox.occ.content.gitignore_oracle import GitignoreMatcher
from sandbox.occ.routing.orchestrator import OccOrchestrator
from sandbox.occ.ports import OccLayerStackPorts
from sandbox.occ.routing.runtime_ops import infer_manifest_base_hash
from sandbox.occ.merge.serial import OccSerialMerger
from sandbox.runtime.async_bridge import run_sync_in_executor


AUTO_SQUASH_MODE_ENV = "EOS_OCC_SQUASH_MODE"
AUTO_SQUASH_MAX_DEPTH_ENV = "EOS_OCC_AUTO_SQUASH_MAX_DEPTH"
AUTO_SQUASH_MAX_DEPTH = 32
AUTO_SQUASH_DRAIN_TIMEOUT_S = 10.0
SquashMode = Literal["sync", "coalesced", "async"]


@dataclass
class _CoalescedSquashState:
    lock: threading.Lock = field(default_factory=threading.Lock)
    state_lock: threading.Lock = field(default_factory=threading.Lock)
    pending_recheck: bool = False


@dataclass(frozen=True)
class _AsyncSquashRequest:
    queued_at_s: float
    depth_before: int


@dataclass(frozen=True)
class AutoSquashMaintenanceRecord:
    error_type: str
    message: str
    queued_at_s: float
    failed_at_s: float

    def to_dict(self) -> dict[str, object]:
        return {
            "error_type": self.error_type,
            "message": self.message,
            "queued_at_s": self.queued_at_s,
            "failed_at_s": self.failed_at_s,
        }


@dataclass
class _AsyncSquashState:
    queue: asyncio.Queue[_AsyncSquashRequest] | None = None
    worker: asyncio.Task[None] | None = None
    worker_lock: asyncio.Lock | None = None
    loop: asyncio.AbstractEventLoop | None = None
    maintenance_records: list[AutoSquashMaintenanceRecord] = field(
        default_factory=list
    )


class OccService:
    """Prepare typed OCC changesets and commit them through the layer stack."""

    def __init__(
        self,
        *,
        gitignore: GitignoreMatcher,
        layer_stack: OccLayerStackPorts,
    ) -> None:
        self._layer_stack = layer_stack
        self._orchestrator = OccOrchestrator(gitignore)
        self._transaction = OccCommitTransaction(
            snapshot_reader=layer_stack,
            staging=layer_stack,
            publisher=layer_stack,
        )
        self._serial_merger = OccSerialMerger(self._transaction)
        self._auto_squash_max_depth = _configured_auto_squash_max_depth()
        self._squash_mode = _configured_squash_mode()
        self._coalesced_squash = _CoalescedSquashState()
        self._async_squash = _AsyncSquashState()

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult:
        """Prepare a changeset and commit it through the layer stack."""
        total_start = time.perf_counter()
        prepared = await self.prepare_changeset(
            changes,
            snapshot=snapshot,
            options=options,
        )
        return await self.commit_prepared(prepared, _total_start=total_start)

    async def commit_prepared(
        self,
        prepared: PreparedChangeset,
        *,
        _total_start: float | None = None,
    ) -> ChangesetResult:
        """Commit an already-prepared changeset through the serial merger.

        The merger's transaction calls ``transaction.snapshot()`` under the
        commit lock and revalidates against the *live* active manifest, so a
        prepared changeset whose ``snapshot`` lags the active manifest is
        validated like any concurrent commit: gated paths whose base hash no
        longer matches receive a normal OCC rejection. Callers may therefore
        run :meth:`prepare_changeset` lock-free and only serialize this call.
        """
        total_start = _total_start if _total_start is not None else time.perf_counter()
        commit_start = time.perf_counter()
        result = await self._serial_merger.apply(prepared)
        commit_elapsed = time.perf_counter() - commit_start
        auto_squash_timings = await self._auto_squash_after_publish(result)
        return self._wrap_commit_result(
            result,
            prepared=prepared,
            total_start=total_start,
            commit_elapsed=commit_elapsed,
            sync=False,
            extra_timings=auto_squash_timings,
        )

    def apply_changeset_sync(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult:
        total_start = time.perf_counter()
        prepared = self.prepare_changeset_sync(
            changes,
            snapshot=snapshot,
            options=options,
        )
        return self.commit_prepared_sync(prepared, _total_start=total_start)

    def commit_prepared_sync(
        self,
        prepared: PreparedChangeset,
        *,
        _total_start: float | None = None,
    ) -> ChangesetResult:
        """Synchronous twin of :meth:`commit_prepared`."""
        total_start = _total_start if _total_start is not None else time.perf_counter()
        commit_start = time.perf_counter()
        result = self._serial_merger.apply_sync(prepared)
        commit_elapsed = time.perf_counter() - commit_start
        auto_squash_timings = self._auto_squash_after_publish_sync(result)
        return self._wrap_commit_result(
            result,
            prepared=prepared,
            total_start=total_start,
            commit_elapsed=commit_elapsed,
            sync=True,
            extra_timings=auto_squash_timings,
        )

    async def _auto_squash_after_publish(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
        if self._squash_mode == "async":
            return await self._enqueue_auto_squash_after_publish(result)
        return cast(
            dict[str, float],
            await run_sync_in_executor(
                self._auto_squash_after_publish_sync,
                result,
            ),
        )

    def _auto_squash_after_publish_sync(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
        if self._squash_mode == "coalesced":
            return self._auto_squash_after_publish_coalesced_sync(result)
        return self._run_auto_squash_if_needed_sync(result)

    def _auto_squash_after_publish_coalesced_sync(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
        context = self._auto_squash_context_sync(result)
        if context is None:
            return {}
        squash, active = context
        if active.depth <= self._auto_squash_max_depth:
            return {}

        state = self._coalesced_squash
        if not state.lock.acquire(blocking=False):
            with state.state_lock:
                state.pending_recheck = True
            timings = {
                "layer_stack.auto_squash.skipped_in_flight": 1.0,
                "layer_stack.auto_squash.max_depth": float(
                    self._auto_squash_max_depth
                ),
                "layer_stack.auto_squash.depth_before": float(active.depth),
            }
            if active.depth <= self._auto_squash_max_depth * 2:
                return timings

            wait_start = time.perf_counter()
            state.lock.acquire()
            timings["layer_stack.auto_squash.backpressure_wait_s"] = (
                time.perf_counter() - wait_start
            )
            try:
                context = self._auto_squash_context_sync(result)
                if context is None:
                    return timings
                squash, active = context
                if active.depth <= self._auto_squash_max_depth:
                    return timings
                return _merge_auto_squash_timings(
                    timings,
                    self._run_squash_for_active_sync(squash, active),
                )
            finally:
                state.lock.release()

        try:
            timings = self._run_squash_for_active_sync(squash, active)
            with state.state_lock:
                pending_recheck = state.pending_recheck
                state.pending_recheck = False
            if not pending_recheck:
                return timings

            context = self._auto_squash_context_sync(result)
            if context is None:
                return timings
            squash, active = context
            if active.depth <= self._auto_squash_max_depth:
                return timings
            recheck_timings = self._run_squash_for_active_sync(squash, active)
            recheck_timings["layer_stack.auto_squash.recheck_triggered"] = 1.0
            return _merge_auto_squash_timings(timings, recheck_timings)
        finally:
            state.lock.release()

    async def _enqueue_auto_squash_after_publish(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
        context = await run_sync_in_executor(self._auto_squash_context_sync, result)
        if context is None:
            return {}
        _squash, active = context
        if active.depth <= self._auto_squash_max_depth:
            return {}

        queue = self._ensure_async_squash_worker()
        queued_at = time.perf_counter()
        queue.put_nowait(
            _AsyncSquashRequest(
                queued_at_s=queued_at,
                depth_before=active.depth,
            )
        )
        return {
            "layer_stack.auto_squash.enqueued": 1.0,
            "layer_stack.auto_squash.queue_depth": float(queue.qsize()),
            "layer_stack.auto_squash.max_depth": float(
                self._auto_squash_max_depth
            ),
            "layer_stack.auto_squash.depth_before": float(active.depth),
        }

    def _ensure_async_squash_worker(
        self,
    ) -> asyncio.Queue[_AsyncSquashRequest]:
        loop = asyncio.get_running_loop()
        state = self._async_squash
        if state.loop is not loop:
            if (
                state.queue is not None
                and state.queue.qsize() > 0
                and state.worker is not None
                and not state.worker.done()
            ):
                raise RuntimeError(
                    "async auto-squash worker cannot move between active event loops"
                )
            state.loop = loop
            state.queue = asyncio.Queue()
            state.worker_lock = asyncio.Lock()
            state.worker = None

        if state.queue is None:
            state.queue = asyncio.Queue()
        if state.worker_lock is None:
            state.worker_lock = asyncio.Lock()
        if state.worker is None or state.worker.done():
            state.worker = loop.create_task(
                self._async_auto_squash_worker(state),
                name="occ-auto-squash-maintenance",
            )
        return state.queue

    async def _async_auto_squash_worker(
        self,
        state: _AsyncSquashState,
    ) -> None:
        assert state.queue is not None
        assert state.worker_lock is not None
        queue = state.queue
        while True:
            first = await queue.get()
            requests = [first]
            while True:
                try:
                    requests.append(queue.get_nowait())
                except asyncio.QueueEmpty:
                    break
            try:
                async with state.worker_lock:
                    await run_sync_in_executor(
                        self._run_auto_squash_if_needed_sync,
                        None,
                    )
            except Exception as exc:  # noqa: BLE001 - maintenance failures are recorded.
                self._record_auto_squash_maintenance_failure(
                    exc,
                    queued_at_s=min(request.queued_at_s for request in requests),
                )
            finally:
                for _ in requests:
                    queue.task_done()

    async def drain_auto_squash_maintenance(
        self,
        *,
        timeout_s: float = AUTO_SQUASH_DRAIN_TIMEOUT_S,
    ) -> dict[str, object]:
        state = self._async_squash
        queue = state.queue
        timed_out = False
        if queue is not None:
            try:
                await asyncio.wait_for(queue.join(), timeout=timeout_s)
            except asyncio.TimeoutError:
                timed_out = True
        return {
            **self.auto_squash_maintenance_status(),
            "drain_timed_out": timed_out,
        }

    def auto_squash_maintenance_status(self) -> dict[str, object]:
        state = self._async_squash
        last_error = (
            state.maintenance_records[-1].to_dict()
            if state.maintenance_records
            else None
        )
        return {
            "mode": self._squash_mode,
            "max_depth": self._auto_squash_max_depth,
            "queue_depth": state.queue.qsize() if state.queue is not None else 0,
            "maintenance_errors": len(state.maintenance_records),
            "last_maintenance_error": last_error,
        }

    def _record_auto_squash_maintenance_failure(
        self,
        exc: Exception,
        *,
        queued_at_s: float,
    ) -> None:
        self._async_squash.maintenance_records.append(
            AutoSquashMaintenanceRecord(
                error_type=type(exc).__name__,
                message=str(exc),
                queued_at_s=queued_at_s,
                failed_at_s=time.perf_counter(),
            )
        )

    def _run_auto_squash_if_needed_sync(
        self,
        result: ChangesetResult | None,
    ) -> dict[str, float]:
        context = self._auto_squash_context_sync(result)
        if context is None:
            return {}
        squash, active = context
        if active.depth <= self._auto_squash_max_depth:
            return {}
        return self._run_squash_for_active_sync(squash, active)

    def _auto_squash_context_sync(
        self,
        result: ChangesetResult | None,
    ):
        if result is not None and result.published_manifest_version is None:
            return None
        squash = getattr(self._layer_stack, "squash", None)
        if not callable(squash):
            return None
        return squash, self._layer_stack.read_active_manifest()

    def _run_squash_for_active_sync(
        self,
        squash,
        active: Manifest,
    ) -> dict[str, float]:
        squash_start = time.perf_counter()
        squashed = squash(max_depth=self._auto_squash_max_depth)
        elapsed = time.perf_counter() - squash_start
        timings = {
            "layer_stack.auto_squash.total_s": elapsed,
            "layer_stack.auto_squash.max_depth": float(
                self._auto_squash_max_depth
            ),
            "layer_stack.auto_squash.depth_before": float(active.depth),
        }
        if squashed is None:
            timings["layer_stack.auto_squash.raced"] = 1.0
            return timings
        timings["layer_stack.auto_squash.depth_after"] = float(squashed.depth)
        timings["layer_stack.auto_squash.manifest_version"] = float(squashed.version)
        return timings

    def _wrap_commit_result(
        self,
        result: ChangesetResult,
        *,
        prepared: PreparedChangeset,
        total_start: float,
        commit_elapsed: float,
        sync: bool,
        extra_timings: dict[str, float] | None = None,
    ) -> ChangesetResult:
        result_timings, resume_wait = _result_timings_with_resume(result)
        timings = {
            **result_timings,
            **(extra_timings or {}),
            "occ.apply.commit_queue_wait_s": result_timings.get(
                "occ.serial.queue_wait_s",
                0.0,
            ),
            "occ.apply.commit_worker_s": result_timings.get(
                "occ.commit.total_s",
                0.0,
            ),
            "occ.apply.commit_resume_wait_s": 0.0 if sync else resume_wait,
            "occ.apply.commit_s": commit_elapsed,
            "occ.apply.total_s": time.perf_counter() - total_start,
        }
        manifest_lag = _manifest_lag(prepared.snapshot, result.published_manifest_version)
        if manifest_lag is not None:
            timings["occ.apply.manifest_lag"] = manifest_lag
        return ChangesetResult(
            files=result.files,
            timings=timings,
            published_manifest_version=result.published_manifest_version,
        )

    async def prepare_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> PreparedChangeset:
        """Route changes and infer leased-snapshot base hashes."""
        return cast(
            PreparedChangeset,
            await run_sync_in_executor(
                self.prepare_changeset_sync,
                changes,
                snapshot=snapshot,
                options=options,
            ),
        )

    def prepare_changeset_sync(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> PreparedChangeset:
        """Route changes and infer leased-snapshot base hashes synchronously."""
        total_start = time.perf_counter()
        timings: dict[str, float] = {}
        commit_options = options or CommitOptions()
        effective_snapshot = snapshot
        if effective_snapshot is None:
            snapshot_start = time.perf_counter()
            effective_snapshot = self._layer_stack.read_active_manifest()
            timings["occ.prepare.current_snapshot_s"] = (
                time.perf_counter() - snapshot_start
            )
        assert effective_snapshot is not None
        layer_stack = self._layer_stack

        def base_hash_reader(path: str) -> str | None:
            return infer_manifest_base_hash(
                snapshot_reader=layer_stack,
                manifest=effective_snapshot,
                path=path,
            )

        prepare_start = time.perf_counter()
        prepared = self._orchestrator.prepare_sync(
            changes,
            snapshot=effective_snapshot,
            options=commit_options,
            base_hash_reader=base_hash_reader,
        )
        timings["occ.prepare.route_and_base_hash_s"] = (
            time.perf_counter() - prepare_start
        )
        timings["occ.prepare.total_s"] = time.perf_counter() - total_start
        return replace(prepared, timings={**prepared.timings, **timings})


def _manifest_lag(
    snapshot: Manifest | None, published_version: int | None
) -> int | None:
    if snapshot is None or published_version is None:
        return None
    delta = published_version - snapshot.version - 1
    return max(0, delta)


def _result_timings_with_resume(result: ChangesetResult) -> tuple[dict[str, float], float]:
    timings = dict(result.timings)
    ready_at = timings.pop("_occ.serial.result_ready_at_s", None)
    if ready_at is None:
        return timings, 0.0
    return timings, max(0.0, time.perf_counter() - ready_at)


def _configured_auto_squash_max_depth() -> int:
    raw = os.getenv(AUTO_SQUASH_MAX_DEPTH_ENV)
    if raw is None or raw.strip() == "":
        return int(AUTO_SQUASH_MAX_DEPTH)
    try:
        depth = int(raw)
    except ValueError as exc:
        raise ValueError(
            f"{AUTO_SQUASH_MAX_DEPTH_ENV} must be a positive integer"
        ) from exc
    if depth < 1:
        raise ValueError(f"{AUTO_SQUASH_MAX_DEPTH_ENV} must be >= 1")
    return depth


def _configured_squash_mode() -> SquashMode:
    mode = os.getenv(AUTO_SQUASH_MODE_ENV, "sync").strip().lower() or "sync"
    if mode not in {"sync", "coalesced", "async"}:
        raise ValueError(
            f"{AUTO_SQUASH_MODE_ENV} must be one of: sync, coalesced, async"
        )
    return cast(SquashMode, mode)


def _merge_auto_squash_timings(
    first: dict[str, float],
    second: dict[str, float],
) -> dict[str, float]:
    if not first:
        return dict(second)
    if not second:
        return dict(first)
    merged = {**first, **second}
    if (
        "layer_stack.auto_squash.total_s" in first
        or "layer_stack.auto_squash.total_s" in second
    ):
        merged["layer_stack.auto_squash.total_s"] = first.get(
            "layer_stack.auto_squash.total_s",
            0.0,
        ) + second.get("layer_stack.auto_squash.total_s", 0.0)
    if "layer_stack.auto_squash.depth_before" in first:
        merged["layer_stack.auto_squash.depth_before"] = first[
            "layer_stack.auto_squash.depth_before"
        ]
    return merged


__all__ = [
    "AUTO_SQUASH_MAX_DEPTH",
    "AUTO_SQUASH_MAX_DEPTH_ENV",
    "AUTO_SQUASH_MODE_ENV",
    "AutoSquashMaintenanceRecord",
    "OccService",
]
