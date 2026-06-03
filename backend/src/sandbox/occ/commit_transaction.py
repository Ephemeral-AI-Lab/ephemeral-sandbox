"""Atomic OCC validation and layer publish transaction."""

from __future__ import annotations

import os
import shutil
from pathlib import Path
from types import TracebackType
from uuid import uuid4

from sandbox._shared.clock import monotonic_now
from sandbox._shared.timing_keys import TimingKey
from sandbox.layer_stack.changes import LayerChange, WriteLayerChange
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset import (
    ChangesetResult,
    ChangeSource,
    FileResult,
    FileStatus,
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
    drop_or_reject_file_result,
)
from sandbox.occ.content_hashing import ContentHasher
from sandbox.occ.ports import (
    LayerCommitPublisher,
    LayerCommitTransaction,
    LayerCommitStagingAllocator,
    LayerSnapshotReader,
)
from sandbox.occ.path_staging import (
    DirectStager,
    GatedStager,
    StagedLayerChanges,
)

# Below this threshold, a buffered Python read+write is cheaper than
# shutil.copyfile (which pays open/sendfile/close per call). Above it,
# the kernel-level copy wins. Crossover measured between 4 KiB (even)
# and 64 KiB (clear win); 16 KiB is conservative on the win side.
_SMALL_FILE_BYTES_THRESHOLD = 16 * 1024


class CommitTransaction:
    """Revalidate prepared OCC path groups and publish one immutable layer."""

    def __init__(
        self,
        *,
        snapshot_reader: LayerSnapshotReader,
        staging: LayerCommitStagingAllocator,
        publisher: LayerCommitPublisher,
    ) -> None:
        self._staging = staging
        self._publisher = publisher
        self._hasher = ContentHasher()
        self._direct_stager = DirectStager(snapshot_reader)
        self._gated_stager = GatedStager(snapshot_reader, hasher=self._hasher)

    def revalidate_and_publish(self, prepared: PreparedChangeset) -> ChangesetResult:
        """Validate against the current active manifest and publish accepted deltas."""
        total_start = monotonic_now()
        timings: dict[str, float] = {}
        with self._publisher.begin_transaction() as transaction:
            timings[TimingKey.LAYER_TRANSACTION_LOCK_WAIT] = transaction.lock_wait_s
            snapshot_start = monotonic_now()
            active_manifest = transaction.snapshot()
            timings[TimingKey.COMMIT_SNAPSHOT] = monotonic_now() - snapshot_start
            with _FileSystemLayerChangeStager(
                self._staging,
                hasher=self._hasher,
            ) as stager:
                validate_start = monotonic_now()
                validations: list[_Validation] = []
                for group in prepared.path_groups:
                    result, accepted_delta = self._validate_group(
                        group,
                        active_manifest=active_manifest,
                        stager=stager,
                    )
                    validations.append((group, result, accepted_delta))
                timings[TimingKey.COMMIT_VALIDATE_GROUPS] = monotonic_now() - validate_start
                occ_gated_failed = _accumulate_route_timings(timings, validations)

                files = tuple(result for _, result, _ in validations)
                drop_message = _atomic_or_overlay_dropped(
                    prepared=prepared,
                    files=files,
                    occ_gated_failed=occ_gated_failed,
                )
                if drop_message is not None:
                    return ChangesetResult(
                        files=tuple(
                            FileResult(
                                path=result.path,
                                status=FileStatus.DROPPED,
                                message=drop_message,
                                timings=result.timings,
                            )
                            if result.status is FileStatus.ACCEPTED
                            else result
                            for result in files
                        ),
                        timings=_finish_timings(timings, total_start, transaction),
                        published_manifest_version=None,
                    )

                collect_start = monotonic_now()
                changes = tuple(
                    change
                    for _, _, accepted_delta in validations
                    if accepted_delta is not None
                    for change in accepted_delta
                )
                timings[TimingKey.COMMIT_COLLECT_CHANGES] = monotonic_now() - collect_start
                if not changes:
                    return ChangesetResult(
                        files=files,
                        timings=_finish_timings(timings, total_start, transaction),
                        published_manifest_version=None,
                    )

                timings[TimingKey.COMMIT_STAGER_WRITE_TOTAL] = stager.write_total_s
                timings[TimingKey.COMMIT_STAGER_WRITE_COUNT] = float(stager.write_count)
                publish_start = monotonic_now()
                published = transaction.publish_layer(
                    changes,
                    source_root=stager.staging_path,
                    timings=timings,
                )
                timings[TimingKey.COMMIT_PUBLISH_LAYER] = monotonic_now() - publish_start
                return ChangesetResult(
                    files=files,
                    timings=_finish_timings(timings, total_start, transaction),
                    published_manifest_version=published.version,
                )

    def _validate_group(
        self,
        group: PreparedPathGroup,
        *,
        active_manifest: Manifest,
        stager: _FileSystemLayerChangeStager,
    ) -> tuple[FileResult, StagedLayerChanges | None]:
        drop_or_reject = drop_or_reject_file_result(group)
        if drop_or_reject is not None:
            return (drop_or_reject, None)
        if group.route is RouteDecision.DIRECT:
            route_stager: DirectStager | GatedStager = self._direct_stager
        elif group.route is RouteDecision.GATED:
            route_stager = self._gated_stager
        else:
            raise AssertionError(f"unhandled route after drop/reject filter: {group.route}")
        return route_stager.stage_group(
            group,
            active_manifest=active_manifest,
            stage_write=stager.write,
            stage_write_from_path=stager.write_from_path,
        )


class _FileSystemLayerChangeStager:
    def __init__(
        self,
        staging: LayerCommitStagingAllocator,
        *,
        hasher: ContentHasher,
    ) -> None:
        self._staging = staging
        self._hasher = hasher
        self._counter = 0
        self._staging_id: str | None = None
        self._staging_path: Path | None = None
        self._write_total_s = 0.0
        self._write_count = 0

    @property
    def write_total_s(self) -> float:
        return self._write_total_s

    @property
    def write_count(self) -> int:
        return self._write_count

    @property
    def staging_path(self) -> Path | None:
        return self._staging_path

    def __enter__(self) -> _FileSystemLayerChangeStager:
        area = self._staging.allocate_commit_staging(uuid4().hex)
        self._staging_id = area.staging_id
        self._staging_path = area.path
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        del exc_type, exc, traceback
        if self._staging_id is not None:
            self._staging.drop_commit_staging(self._staging_id)
            self._staging_id = None
            self._staging_path = None

    def write(self, path: str, content: bytes) -> LayerChange:
        if self._staging_path is None:
            raise RuntimeError("OCC layer-change stager is not active")
        start = monotonic_now()
        try:
            self._counter += 1
            source = self._staging_path / f"{self._counter:06d}.bin"
            source.write_bytes(content)
            return WriteLayerChange(
                path=path,
                content_hash=self._hasher.hash_bytes(content),
                source_path=str(source),
            )
        finally:
            self._write_total_s += monotonic_now() - start
            self._write_count += 1

    def write_from_path(
        self,
        path: str,
        content_path: str,
        precomputed_hash: str,
        cached_bytes: bytes | None = None,
    ) -> LayerChange:
        """Stage from an existing on-disk file.

        Caller guarantees ``precomputed_hash`` matches the file at
        ``content_path`` — the stager reuses it instead of recomputing.
        ``cached_bytes`` short-circuits the small-file disk read when
        the merge layer already loaded the bytes for the hash chain.
        """
        if self._staging_path is None:
            raise RuntimeError("OCC layer-change stager is not active")
        start = monotonic_now()
        try:
            self._counter += 1
            source = self._staging_path / f"{self._counter:06d}.bin"
            if cached_bytes is not None:
                source.write_bytes(cached_bytes)
            elif os.path.getsize(content_path) >= _SMALL_FILE_BYTES_THRESHOLD:
                shutil.copyfile(content_path, source)
            else:
                source.write_bytes(Path(content_path).read_bytes())
            return WriteLayerChange(
                path=path,
                content_hash=precomputed_hash,
                source_path=str(source),
            )
        finally:
            self._write_total_s += monotonic_now() - start
            self._write_count += 1


_FAILURE_STATUSES = frozenset(
    {
        FileStatus.ABORTED_OVERLAP,
        FileStatus.ABORTED_VERSION,
        FileStatus.FAILED,
        FileStatus.REJECTED,
    }
)

_Validation = tuple[PreparedPathGroup, FileResult, StagedLayerChanges | None]


def _accumulate_route_timings(
    timings: dict[str, float],
    validations: list[_Validation],
) -> bool:
    occ_gated_failed = False
    gated_read_total = 0.0
    gated_apply_total = 0.0
    gated_stage_total = 0.0
    direct_read_total = 0.0
    direct_apply_total = 0.0
    direct_stage_total = 0.0
    gated_count = 0
    direct_count = 0
    for group, result, _ in validations:
        if group.route is RouteDecision.GATED:
            gated_count += 1
            if result.status in _FAILURE_STATUSES:
                occ_gated_failed = True
            route_timings = result.timings
            gated_read_total += route_timings.get(TimingKey.GATED_READ_CURRENT, 0.0)
            gated_apply_total += route_timings.get(TimingKey.GATED_APPLY_CHANGES, 0.0)
            gated_stage_total += route_timings.get(TimingKey.GATED_STAGE_DELTA, 0.0)
        elif group.route is RouteDecision.DIRECT:
            direct_count += 1
            route_timings = result.timings
            direct_read_total += route_timings.get(TimingKey.DIRECT_READ_CURRENT, 0.0)
            direct_apply_total += route_timings.get(TimingKey.DIRECT_APPLY_CHANGES, 0.0)
            direct_stage_total += route_timings.get(TimingKey.DIRECT_STAGE_DELTA, 0.0)

    timings[TimingKey.COMMIT_GATED_READ_TOTAL] = gated_read_total
    timings[TimingKey.COMMIT_GATED_APPLY_TOTAL] = gated_apply_total
    timings[TimingKey.COMMIT_GATED_STAGE_TOTAL] = gated_stage_total
    timings[TimingKey.COMMIT_GATED_PATH_COUNT] = float(gated_count)
    timings[TimingKey.COMMIT_DIRECT_READ_TOTAL] = direct_read_total
    timings[TimingKey.COMMIT_DIRECT_APPLY_TOTAL] = direct_apply_total
    timings[TimingKey.COMMIT_DIRECT_STAGE_TOTAL] = direct_stage_total
    timings[TimingKey.COMMIT_DIRECT_PATH_COUNT] = float(direct_count)
    return occ_gated_failed


def _atomic_or_overlay_dropped(
    *,
    prepared: PreparedChangeset,
    files: tuple[FileResult, ...],
    occ_gated_failed: bool,
) -> str | None:
    if prepared.atomic and any(result.status in _FAILURE_STATUSES for result in files):
        return "not published because atomic changeset validation failed"
    if occ_gated_failed and any(
        change.source is ChangeSource.OVERLAY_CAPTURE
        for group in prepared.path_groups
        for change in group.changes
    ):
        return "not published because overlay capture OCC-gated validation failed"
    return None


def _finish_timings(
    timings: dict[str, float],
    total_start: float,
    transaction: LayerCommitTransaction,
) -> dict[str, float]:
    return {
        **timings,
        TimingKey.COMMIT_TOTAL: monotonic_now() - total_start,
        TimingKey.LAYER_TRANSACTION_LOCK_HELD: float(transaction.lock_held_s),
    }


__all__ = [
    "CommitTransaction",
]
