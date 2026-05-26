"""OCC changeset preparation and commit service."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import replace
from typing import cast

from sandbox._shared.clock import monotonic_now
from sandbox._shared.timing_keys import TimingKey
from sandbox.daemon.audit_schema import (
    OccSection,
    build_occ_event,
    safe_emit,
    safe_record_phase,
)
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset import CommitOptions, PreparedChangeset
from sandbox.occ.changeset import Change, ChangesetResult, FileStatus
from sandbox.occ.changeset_preparation import ChangesetPreparer
from sandbox.occ.commit_queue import CommitQueue
from sandbox.occ.commit_transaction import CommitTransaction
from sandbox.occ.content_hashing import infer_snapshot_base_hash
from sandbox.occ.gitignore import GitignoreMatcher
from sandbox.occ.maintenance import MaintenancePolicy
from sandbox.occ.ports import OccLayerStackPort
from sandbox._shared.async_bridge import run_sync_in_executor

AUTO_SQUASH_MAX_DEPTH = 100


class OccService:
    """Prepare typed OCC changesets and commit them through the layer stack."""

    def __init__(
        self,
        *,
        gitignore: GitignoreMatcher,
        layer_stack: OccLayerStackPort,
        preparer: ChangesetPreparer | None = None,
        transaction: CommitTransaction | None = None,
        commit_queue: CommitQueue | None = None,
        maintenance: MaintenancePolicy | None = None,
    ) -> None:
        self._snapshot_reader = layer_stack
        self._preparer = preparer or ChangesetPreparer(gitignore)
        transaction = transaction or CommitTransaction(
            snapshot_reader=layer_stack,
            staging=layer_stack,
            publisher=layer_stack,
        )
        self._owns_commit_queue = commit_queue is None
        self._commit_queue = commit_queue or CommitQueue(transaction)
        if self._owns_commit_queue:
            self._commit_queue.start()
        self._maintenance: MaintenancePolicy | None = maintenance

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
        run_maintenance: bool = True,
    ) -> ChangesetResult:
        """Prepare a changeset and commit it through the layer stack."""
        total_start = monotonic_now()
        prepared = await self.prepare_changeset(
            changes,
            snapshot=snapshot,
            options=options,
        )
        return await self.commit_prepared(
            prepared,
            _total_start=total_start,
            run_maintenance=run_maintenance,
        )

    async def commit_prepared(
        self,
        prepared: PreparedChangeset,
        *,
        _total_start: float | None = None,
        run_maintenance: bool = True,
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
        maintenance_timings = (
            await self._maintenance_after_publish(result) if run_maintenance else {}
        )
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
        run_maintenance: bool = True,
    ) -> ChangesetResult:
        """Synchronous twin of :meth:`apply_changeset`."""
        total_start = monotonic_now()
        prepared = self.prepare_changeset_sync(
            changes,
            snapshot=snapshot,
            options=options,
        )
        return self.commit_prepared_sync(
            prepared,
            _total_start=total_start,
            run_maintenance=run_maintenance,
        )

    def commit_prepared_sync(
        self,
        prepared: PreparedChangeset,
        *,
        _total_start: float | None = None,
        run_maintenance: bool = True,
    ) -> ChangesetResult:
        """Synchronous twin of :meth:`commit_prepared`."""
        total_start = _total_start if _total_start is not None else monotonic_now()
        commit_start = monotonic_now()
        result = self._commit_queue.apply_sync(prepared)
        commit_elapsed = monotonic_now() - commit_start
        maintenance_timings = (
            self._maintenance_after_publish_sync(result) if run_maintenance else {}
        )
        return self._wrap_commit_result(
            result,
            prepared=prepared,
            total_start=total_start,
            commit_elapsed=commit_elapsed,
            sync_call=True,
            extra_timings=maintenance_timings,
        )

    async def run_maintenance_after_publish(
        self,
        result: ChangesetResult,
    ) -> dict[str, float]:
        return await self._maintenance_after_publish(result)

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
        wrapped = ChangesetResult(
            files=result.files,
            timings=timings,
            published_manifest_version=published,
        )
        _emit_occ_commit_events(
            wrapped,
            prepared=prepared,
            commit_elapsed=commit_elapsed,
        )
        return wrapped

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
            return infer_snapshot_base_hash(
                snapshot_reader=snapshot_reader,
                manifest=effective_snapshot,
                path=path,
            )

        prepare_start = monotonic_now()
        prepared = self._preparer.prepare_sync(
            changes,
            snapshot=effective_snapshot,
            options=commit_options,
            base_hash_reader=base_hash_reader,
        )
        timings[TimingKey.PREPARE_ROUTE_AND_BASE_HASH] = monotonic_now() - prepare_start
        prepare_total_s = monotonic_now() - total_start
        timings[TimingKey.PREPARE_TOTAL] = prepare_total_s
        prepared = replace(prepared, timings={**prepared.timings, **timings})
        safe_emit(
            build_occ_event(
                "occ.changeset_prepared",
                OccSection(
                    operation_step=70,
                    changeset_id=prepared.changeset_id or None,
                    changed_path_count=sum(
                        len(group.changes) for group in prepared.path_groups
                    ),
                    prepare_ms=prepare_total_s * 1000.0,
                    base_manifest_version=(
                        prepared.snapshot.version
                        if prepared.snapshot is not None
                        else None
                    ),
                ),
            ),
            lane="normal",
        )
        return prepared

    def close(self) -> None:
        """Stop owned background resources."""
        if self._owns_commit_queue:
            self._commit_queue.close()


def _emit_occ_commit_events(
    result: ChangesetResult,
    *,
    prepared: PreparedChangeset,
    commit_elapsed: float,
) -> None:
    """Emit ``occ.apply_committed`` / ``occ.publish_layer`` / ``occ.conflict_rejected``.

    Called from ``_wrap_commit_result`` so the timings + manifest version are
    already populated on ``result``.
    """
    operation_id = getattr(prepared, "request_id", None) or None
    changeset_id = prepared.changeset_id or None
    base_version = (
        prepared.snapshot.version if prepared.snapshot is not None else None
    )
    bad = next(
        (f for f in result.files if f.status != FileStatus.COMMITTED),
        None,
    )
    if bad is not None:
        safe_emit(
            build_occ_event(
                "occ.conflict_rejected",
                OccSection(
                    operation_id=operation_id,
                    changeset_id=changeset_id,
                    conflict_kind=bad.status.value,
                    conflict_path=bad.path or None,
                    conflict_reason=(bad.message or bad.status.value) or None,
                    base_manifest_version=base_version,
                    current_manifest_version=result.published_manifest_version,
                ),
            ),
            lane="critical",
        )
        return
    apply_ms = commit_elapsed * 1000.0
    safe_emit(
        build_occ_event(
            "occ.apply_committed",
            OccSection(
                operation_id=operation_id,
                operation_step=110,
                changeset_id=changeset_id,
                changed_path_count=len(result.files),
                apply_ms=apply_ms,
                commit_ms=apply_ms,
                base_manifest_version=base_version,
                current_manifest_version=result.published_manifest_version,
            ),
        ),
        lane="normal",
    )
    if result.published_manifest_version is not None:
        safe_emit(
            build_occ_event(
                "occ.publish_layer",
                OccSection(
                    operation_id=operation_id,
                    changeset_id=changeset_id,
                    committed_layer_id=str(result.published_manifest_version),
                    current_manifest_version=result.published_manifest_version,
                    publish_layer_ms=apply_ms,
                ),
            ),
            lane="normal",
        )
        # V3 §2/§3 — surface the publish phase in the per-tool rollup. The
        # apply→publish boundary is the full ``commit_elapsed`` window
        # because the underlying transaction publishes synchronously.
        safe_record_phase("publish", apply_ms)


__all__ = [
    "AUTO_SQUASH_MAX_DEPTH",
    "OccService",
]
