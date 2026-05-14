"""OCC changeset preparation and commit service."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import replace
from typing import cast

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.stage.transaction import CommitTransaction
from sandbox.occ.content.gitignore_oracle import GitignoreMatcher
from sandbox.occ.content.hashing import infer_manifest_base_hash
from sandbox.occ.maintenance import MaintenancePolicy, NoopMaintenancePolicy
from sandbox.occ.commit_queue import CommitQueue
from sandbox.occ.ports import CommitPublisher, CommitStagingStore, SnapshotReader
from sandbox.occ.router import Router
from sandbox.occ.timing_keys import TimingKey
from sandbox.async_bridge import run_sync_in_executor
from sandbox.timing import monotonic_now

AUTO_SQUASH_MAX_DEPTH = 32


class Service:
    """Prepare typed OCC changesets and commit them through the layer stack."""

    def __init__(
        self,
        *,
        gitignore: GitignoreMatcher,
        snapshot_reader: SnapshotReader,
        staging: CommitStagingStore,
        publisher: CommitPublisher,
        orchestrator: Router | None = None,
        transaction: CommitTransaction | None = None,
        commit_queue: CommitQueue | None = None,
        maintenance: MaintenancePolicy | None = None,
    ) -> None:
        self._snapshot_reader = snapshot_reader
        self._orchestrator = orchestrator or Router(gitignore)
        self._transaction = transaction or CommitTransaction(
            snapshot_reader=snapshot_reader,
            staging=staging,
            publisher=publisher,
        )
        self._owns_commit_queue = commit_queue is None
        self._commit_queue = commit_queue or CommitQueue(self._transaction)
        if self._owns_commit_queue:
            self._commit_queue.start()
        self._maintenance = maintenance or NoopMaintenancePolicy()

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
        return await run_sync_in_executor(
            self._maintenance.after_publish_sync,
            result,
        )

    def _maintenance_after_publish_sync(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
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
        result_timings, resume_wait = _result_timings_with_resume(result)
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
        manifest_lag = _manifest_lag(prepared.snapshot, result.published_manifest_version)
        if manifest_lag is not None:
            timings[TimingKey.APPLY_MANIFEST_LAG] = manifest_lag
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


def _manifest_lag(snapshot: Manifest | None, published_version: int | None) -> int | None:
    if snapshot is None or published_version is None:
        return None
    delta = published_version - snapshot.version - 1
    return max(0, delta)


def _default_maintenance(
    layer_stack: _LayerStackOccPort | None,
    *,
    max_depth: int,
) -> MaintenancePolicy:
    if layer_stack is None or max_depth < 1 or not isinstance(layer_stack, SquashPort):
        return NoopMaintenancePolicy()
    return AutoSquashMaintenancePolicy(
        snapshot_reader=layer_stack,
        squasher=layer_stack,
        max_depth=max_depth,
    )


def _result_timings_with_resume(result: ChangesetResult) -> tuple[dict[str, float], float]:
    timings = dict(result.timings)
    ready_at = timings.pop(TimingKey.SERIAL_RESULT_READY_AT, None)
    if ready_at is None:
        return timings, 0.0
    return timings, max(0.0, monotonic_now() - ready_at)


__all__ = [
    "AUTO_SQUASH_MAX_DEPTH",
    "Service",
]
