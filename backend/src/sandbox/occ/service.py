"""OCC changeset preparation and commit service."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import replace
from typing import cast

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset import CommitOptions, PreparedChangeset
from sandbox.occ.changeset import Change, ChangesetResult
from sandbox.occ.commit_transaction import CommitTransaction
from sandbox.occ.gitignore import GitignoreMatcher
from sandbox.occ.hashing import infer_manifest_base_hash
from sandbox.occ.maintenance import MaintenancePolicy
from sandbox.occ.commit_queue import CommitQueue
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from sandbox.layer_stack.stack import LayerStack
from sandbox.occ.preparer import ChangesetPreparer
from sandbox.timing_keys import TimingKey
from sandbox.daemon.async_bridge import run_sync_in_executor
from sandbox._shared.clock import monotonic_now

AUTO_SQUASH_MAX_DEPTH = 32


class OccService:
    """Prepare typed OCC changesets and commit them through the layer stack."""

    def __init__(
        self,
        *,
        gitignore: GitignoreMatcher,
        layer_stack: LayerStack,
        orchestrator: ChangesetPreparer | None = None,
        transaction: CommitTransaction | None = None,
        commit_queue: CommitQueue | None = None,
        maintenance: MaintenancePolicy | None = None,
    ) -> None:
        self._snapshot_reader = layer_stack
        self._orchestrator = orchestrator or ChangesetPreparer(gitignore)
        self._transaction = transaction or CommitTransaction(
            snapshot_reader=layer_stack,
            staging=layer_stack,
            publisher=layer_stack,
        )
        self._owns_commit_queue = commit_queue is None
        self._commit_queue = commit_queue or CommitQueue(self._transaction)
        if self._owns_commit_queue:
            self._commit_queue.start()
        self._maintenance: MaintenancePolicy | None = maintenance

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
        """Commit an already-prepared changeset through the commit queue.

        The merger's transaction calls ``transaction.snapshot()`` under the
        commit lock and revalidates against the *live* active manifest, so a
        prepared changeset whose ``snapshot`` lags the active manifest is
        validated like any concurrent commit: gated paths whose base hash no
        longer matches receive a normal OCC rejection. Callers may therefore
        run :meth:`prepare_changeset` lock-free and only serialize this call.
        """
        total_start = _total_start if _total_start is not None else monotonic_now()
        commit_start = monotonic_now()
        result = await self._commit_queue.apply(prepared)
        commit_elapsed = monotonic_now() - commit_start
        maintenance_timings = await self._maintenance_after_publish(result)
        return self._wrap_commit_result(
            result,
            prepared=prepared,
            total_start=total_start,
            commit_elapsed=commit_elapsed,
            sync_call=False,
            extra_timings=maintenance_timings,
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
        result = self._commit_queue.apply_sync(prepared)
        commit_elapsed = monotonic_now() - commit_start
        maintenance_timings = self._maintenance_after_publish_sync(result)
        return self._wrap_commit_result(
            result,
            prepared=prepared,
            total_start=total_start,
            commit_elapsed=commit_elapsed,
            sync_call=True,
            extra_timings=maintenance_timings,
        )

    async def _maintenance_after_publish(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
        if self._maintenance is None:
            return {}
        return await run_sync_in_executor(
            self._maintenance.after_publish_sync,
            result,
        )

    def _maintenance_after_publish_sync(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
        if self._maintenance is None:
            return {}
        return self._maintenance.after_publish_sync(result)

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
        result_timings = dict(result.timings)
        ready_at = result_timings.pop(TimingKey.SERIAL_RESULT_READY_AT, None)
        resume_wait = 0.0 if ready_at is None else max(0.0, monotonic_now() - ready_at)
        timings = {
            **result_timings,
            **(extra_timings or {}),
            TimingKey.APPLY_COMMIT_QUEUE_WAIT: result_timings.get(
                TimingKey.SERIAL_QUEUE_WAIT,
                0.0,
            ),
            TimingKey.APPLY_COMMIT_WORKER: result_timings.get(
                TimingKey.COMMIT_TOTAL,
                0.0,
            ),
            TimingKey.APPLY_COMMIT_RESUME_WAIT: 0.0 if sync_call else resume_wait,
            TimingKey.APPLY_COMMIT: commit_elapsed,
            TimingKey.APPLY_TOTAL: monotonic_now() - total_start,
        }
        published = result.published_manifest_version
        snapshot = prepared.snapshot
        if snapshot is not None and published is not None:
            timings[TimingKey.APPLY_MANIFEST_LAG] = max(0, published - snapshot.version - 1)
        return ChangesetResult(
            files=result.files,
            timings=timings,
            published_manifest_version=published,
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
            effective_snapshot = self._snapshot_reader.read_active_manifest()
            timings[TimingKey.PREPARE_CURRENT_SNAPSHOT] = monotonic_now() - snapshot_start
        assert effective_snapshot is not None
        snapshot_reader = self._snapshot_reader

        def base_hash_reader(path: str) -> str | None:
            return infer_manifest_base_hash(
                snapshot_reader=snapshot_reader,
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
        timings[TimingKey.PREPARE_ROUTE_AND_BASE_HASH] = monotonic_now() - prepare_start
        timings[TimingKey.PREPARE_TOTAL] = monotonic_now() - total_start
        return replace(prepared, timings={**prepared.timings, **timings})

    def close(self) -> None:
        """Stop owned background resources."""
        if self._owns_commit_queue:
            self._commit_queue.close()


__all__ = [
    "AUTO_SQUASH_MAX_DEPTH",
    "OccService",
]
