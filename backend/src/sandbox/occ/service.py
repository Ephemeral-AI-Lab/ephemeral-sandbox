"""OCC changeset preparation and commit service."""

from __future__ import annotations

import threading
from collections.abc import Sequence
from dataclasses import dataclass, field, replace
from typing import cast

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.commit_transaction import OccCommitTransaction
from sandbox.occ.content.gitignore_oracle import GitignoreMatcher
from sandbox.occ.content.hashing import infer_manifest_base_hash
from sandbox.occ.merge.serial import OccSerialMerger
from sandbox.occ.ports import OccLayerStackPorts
from sandbox.occ.routing.orchestrator import OccOrchestrator
from sandbox.async_bridge import run_sync_in_executor
from sandbox.timing import monotonic_now

AUTO_SQUASH_MAX_DEPTH = 32


@dataclass
class _CoalescedSquashState:
    lock: threading.Lock = field(default_factory=threading.Lock)
    state_lock: threading.Lock = field(default_factory=threading.Lock)
    pending_recheck: bool = False


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
        self._auto_squash_max_depth = int(AUTO_SQUASH_MAX_DEPTH)
        self._coalesced_squash = _CoalescedSquashState()

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult:
        """Prepare a changeset and commit it through the layer stack."""
        total_start = monotonic_now()
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
        total_start = _total_start if _total_start is not None else monotonic_now()
        commit_start = monotonic_now()
        result = await self._serial_merger.apply(prepared)
        commit_elapsed = monotonic_now() - commit_start
        auto_squash_timings = await self._auto_squash_after_publish(result)
        return self._wrap_commit_result(
            result,
            prepared=prepared,
            total_start=total_start,
            commit_elapsed=commit_elapsed,
            sync_call=False,
            extra_timings=auto_squash_timings,
        )

    def apply_changeset_sync(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult:
        total_start = monotonic_now()
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
        total_start = _total_start if _total_start is not None else monotonic_now()
        commit_start = monotonic_now()
        result = self._serial_merger.apply_sync(prepared)
        commit_elapsed = monotonic_now() - commit_start
        auto_squash_timings = self._auto_squash_after_publish_sync(result)
        return self._wrap_commit_result(
            result,
            prepared=prepared,
            total_start=total_start,
            commit_elapsed=commit_elapsed,
            sync_call=True,
            extra_timings=auto_squash_timings,
        )

    async def _auto_squash_after_publish(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
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
        return self._auto_squash_after_publish_coalesced_sync(result)

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
            return {
                "layer_stack.auto_squash.skipped_in_flight": 1.0,
                "layer_stack.auto_squash.max_depth": float(
                    self._auto_squash_max_depth
                ),
                "layer_stack.auto_squash.depth_before": float(active.depth),
            }

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
        squash_start = monotonic_now()
        squashed = squash(max_depth=self._auto_squash_max_depth)
        elapsed = monotonic_now() - squash_start
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
        sync_call: bool,
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
            "occ.apply.commit_resume_wait_s": 0.0 if sync_call else resume_wait,
            "occ.apply.commit_s": commit_elapsed,
            "occ.apply.total_s": monotonic_now() - total_start,
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
        total_start = monotonic_now()
        timings: dict[str, float] = {}
        commit_options = options or CommitOptions()
        effective_snapshot = snapshot
        if effective_snapshot is None:
            snapshot_start = monotonic_now()
            effective_snapshot = self._layer_stack.read_active_manifest()
            timings["occ.prepare.current_snapshot_s"] = (
                monotonic_now() - snapshot_start
            )
        assert effective_snapshot is not None
        layer_stack = self._layer_stack

        def base_hash_reader(path: str) -> str | None:
            return infer_manifest_base_hash(
                snapshot_reader=layer_stack,
                manifest=effective_snapshot,
                path=path,
            )

        prepare_start = monotonic_now()
        prepared = self._orchestrator.prepare_sync(
            changes,
            snapshot=effective_snapshot,
            options=commit_options,
            base_hash_reader=base_hash_reader,
        )
        timings["occ.prepare.route_and_base_hash_s"] = (
            monotonic_now() - prepare_start
        )
        timings["occ.prepare.total_s"] = monotonic_now() - total_start
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
    return timings, max(0.0, monotonic_now() - ready_at)


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
    "OccService",
]
