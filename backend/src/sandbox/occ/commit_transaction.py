"""Atomic OCC validation and layer publish transaction."""

from __future__ import annotations

import os
import shutil
import time
from dataclasses import dataclass
from pathlib import Path
from types import TracebackType
from uuid import uuid4

from sandbox.layer_stack.layer.change import LayerChange, LayerDelta
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import (
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
)
from sandbox.occ.changeset.types import (
    ChangesetResult,
    FileResult,
    FileStatus,
)
from sandbox.occ.merge.direct import DirectMerge
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.merge.gated import GatedMerge
from sandbox.occ.ports import (
    CommitPublisher,
    CommitStagingStore,
    OccLayerStackPorts,
    SnapshotReader,
)


# Below this threshold, a buffered Python read+write is cheaper than
# shutil.copyfile (which pays open/sendfile/close per call). Above it,
# the kernel-level copy wins. Crossover measured between 4 KiB (even)
# and 64 KiB (clear win); 16 KiB is conservative on the win side.
_SMALL_FILE_BYTES_THRESHOLD = 16 * 1024


@dataclass(frozen=True)
class PathValidation:
    result: FileResult
    accepted_delta: LayerDelta | None


class OccCommitTransaction:
    """Revalidate prepared OCC path groups and publish one immutable layer."""

    def __init__(
        self,
        layer_stack: OccLayerStackPorts | None = None,
        *,
        snapshot_reader: SnapshotReader | None = None,
        staging: CommitStagingStore | None = None,
        publisher: CommitPublisher | None = None,
    ) -> None:
        if layer_stack is not None:
            snapshot_reader = snapshot_reader or layer_stack
            staging = staging or layer_stack
            publisher = publisher or layer_stack
        if snapshot_reader is None or staging is None or publisher is None:
            raise TypeError(
                "OccCommitTransaction requires snapshot_reader, staging, "
                "and publisher ports"
            )
        self._snapshot_reader = snapshot_reader
        self._staging = staging
        self._publisher = publisher
        self._hasher = ContentHasher()
        self._gated = GatedMerge(snapshot_reader, hasher=self._hasher)
        self._direct = DirectMerge(snapshot_reader)

    def revalidate_and_publish(self, prepared: PreparedChangeset) -> ChangesetResult:
        """Validate against the current active manifest and publish accepted deltas."""
        total_start = time.perf_counter()
        timings: dict[str, float] = {}
        with self._publisher.commit_transaction() as transaction:
            timings["layer_stack.transaction.lock_wait_s"] = transaction.lock_wait_s
            snapshot_start = time.perf_counter()
            active_manifest = transaction.snapshot()
            timings["occ.commit.snapshot_s"] = time.perf_counter() - snapshot_start
            with _LayerChangeStager(
                self._staging,
                hasher=self._hasher,
            ) as stager:
                validate_start = time.perf_counter()
                validations: list[PathValidation] = []
                occ_gated_failed = False
                gated_read_total = 0.0
                gated_apply_total = 0.0
                gated_stage_total = 0.0
                direct_read_total = 0.0
                direct_apply_total = 0.0
                direct_stage_total = 0.0
                gated_count = 0
                direct_count = 0
                for group in prepared.path_groups:
                    validation = self._validate_group(
                        group,
                        active_manifest=active_manifest,
                        stager=stager,
                    )
                    validations.append(validation)
                    if (
                        group.route is RouteDecision.OCC_GATED_MERGE
                        and validation.result.status is not FileStatus.ACCEPTED
                    ):
                        occ_gated_failed = True
                    rt = validation.result.timings
                    if group.route is RouteDecision.OCC_GATED_MERGE:
                        gated_count += 1
                        gated_read_total += rt.get("occ.gated.read_current_s", 0.0)
                        gated_apply_total += rt.get(
                            "occ.gated.apply_changes_s", 0.0
                        )
                        gated_stage_total += rt.get("occ.gated.stage_delta_s", 0.0)
                    elif group.route is RouteDecision.OCC_SKIPPED_MERGE:
                        direct_count += 1
                        direct_read_total += rt.get("occ.direct.read_current_s", 0.0)
                        direct_apply_total += rt.get(
                            "occ.direct.apply_changes_s", 0.0
                        )
                        direct_stage_total += rt.get("occ.direct.stage_delta_s", 0.0)
                timings["occ.commit.validate_groups_s"] = (
                    time.perf_counter() - validate_start
                )
                timings["occ.commit.gated_read_current_total_s"] = gated_read_total
                timings["occ.commit.gated_apply_changes_total_s"] = gated_apply_total
                timings["occ.commit.gated_stage_delta_total_s"] = gated_stage_total
                timings["occ.commit.gated_path_count"] = float(gated_count)
                timings["occ.commit.direct_read_current_total_s"] = direct_read_total
                timings["occ.commit.direct_apply_changes_total_s"] = (
                    direct_apply_total
                )
                timings["occ.commit.direct_stage_delta_total_s"] = direct_stage_total
                timings["occ.commit.direct_path_count"] = float(direct_count)

                files = tuple(validation.result for validation in validations)
                if _must_skip_publish(
                    prepared,
                    files,
                    occ_gated_failed=occ_gated_failed,
                ):
                    return ChangesetResult(
                        files=tuple(_mark_unpublished(files, prepared)),
                        timings=_finish_timings(
                            timings,
                            total_start,
                            transaction=transaction,
                        ),
                        published_manifest_version=None,
                    )

                collect_start = time.perf_counter()
                changes = tuple(
                    change
                    for validation in validations
                    if validation.accepted_delta is not None
                    for change in validation.accepted_delta.changes
                )
                timings["occ.commit.collect_changes_s"] = (
                    time.perf_counter() - collect_start
                )
                if not changes:
                    return ChangesetResult(
                        files=files,
                        timings=_finish_timings(
                            timings,
                            total_start,
                            transaction=transaction,
                        ),
                        published_manifest_version=None,
                    )

                timings["occ.commit.stager_write_total_s"] = (
                    stager.write_total_s
                )
                timings["occ.commit.stager_write_count"] = float(
                    stager.write_count
                )
                publish_start = time.perf_counter()
                published = transaction.publish_layer(changes, timings=timings)
                timings["occ.commit.publish_layer_s"] = (
                    time.perf_counter() - publish_start
                )
                return ChangesetResult(
                    files=files,
                    timings=_finish_timings(
                        timings,
                        total_start,
                        transaction=transaction,
                    ),
                    published_manifest_version=published.version,
                )

    def _validate_group(
        self,
        group: PreparedPathGroup,
        *,
        active_manifest: Manifest,
        stager: "_LayerChangeStager",
    ) -> PathValidation:
        if group.route is RouteDecision.DROP:
            return PathValidation(
                result=FileResult(
                    path=group.path,
                    status=FileStatus.DROPPED,
                    message=group.message or "change dropped",
                ),
                accepted_delta=None,
            )
        if group.route is RouteDecision.REJECT:
            return PathValidation(
                result=FileResult(
                    path=group.path,
                    status=FileStatus.REJECTED,
                    message=group.message or "change rejected",
                ),
                accepted_delta=None,
            )
        if group.route is RouteDecision.OCC_SKIPPED_MERGE:
            result, delta = self._direct.stage_group(
                group,
                active_manifest=active_manifest,
                stage_write=stager.write,
                stage_write_from_path=stager.write_from_path,
            )
            return PathValidation(result=result, accepted_delta=delta)
        if group.route is RouteDecision.OCC_GATED_MERGE:
            result, delta = self._gated.stage_group(
                group,
                active_manifest=active_manifest,
                stage_write=stager.write,
                stage_write_from_path=stager.write_from_path,
            )
            return PathValidation(result=result, accepted_delta=delta)
        return PathValidation(
            result=FileResult(
                path=group.path,
                status=FileStatus.REJECTED,
                message=f"unsupported route: {group.route}",
            ),
            accepted_delta=None,
        )


class _LayerChangeStager:
    def __init__(
        self,
        staging: CommitStagingStore,
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

    def __enter__(self) -> "_LayerChangeStager":
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
        start = time.perf_counter()
        try:
            self._counter += 1
            source = self._staging_path / f"{self._counter:06d}.bin"
            source.write_bytes(content)
            return LayerChange(
                path=path,
                kind="write",
                content_hash=self._hasher.hash_bytes(content),
                source_path=str(source),
            )
        finally:
            self._write_total_s += time.perf_counter() - start
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
        start = time.perf_counter()
        try:
            self._counter += 1
            source = self._staging_path / f"{self._counter:06d}.bin"
            try:
                file_size = os.path.getsize(content_path)
            except OSError:
                file_size = 0
            if file_size >= _SMALL_FILE_BYTES_THRESHOLD:
                shutil.copyfile(content_path, source)
            elif cached_bytes is not None:
                # Cheap consistency guard against a caller passing
                # bytes for a different file than content_path.
                if file_size != len(cached_bytes):
                    raise RuntimeError(
                        "stage_write_from_path cached_bytes length "
                        f"{len(cached_bytes)} disagrees with file size "
                        f"{file_size} at {content_path!r}"
                    )
                source.write_bytes(cached_bytes)
            else:
                source.write_bytes(Path(content_path).read_bytes())
            return LayerChange(
                path=path,
                kind="write",
                content_hash=precomputed_hash,
                source_path=str(source),
            )
        finally:
            self._write_total_s += time.perf_counter() - start
            self._write_count += 1


def _must_skip_publish(
    prepared: PreparedChangeset,
    files: tuple[FileResult, ...],
    *,
    occ_gated_failed: bool,
) -> bool:
    if prepared.atomic and any(_is_failure(result) for result in files):
        return True
    return _is_overlay_capture_changeset(prepared) and occ_gated_failed


def _is_overlay_capture_changeset(prepared: PreparedChangeset) -> bool:
    return any(
        change.source == "overlay_capture"
        for group in prepared.path_groups
        for change in group.changes
    )


def _is_failure(result: FileResult) -> bool:
    return result.status in {
        FileStatus.ABORTED_OVERLAP,
        FileStatus.ABORTED_VERSION,
        FileStatus.FAILED,
        FileStatus.REJECTED,
    }


def _mark_unpublished(
    files: tuple[FileResult, ...],
    prepared: PreparedChangeset,
) -> tuple[FileResult, ...]:
    if prepared.atomic:
        message = "not published because atomic changeset validation failed"
    else:
        message = "not published because overlay capture OCC-gated validation failed"

    marked: list[FileResult] = []
    for result in files:
        if result.status is FileStatus.ACCEPTED:
            marked.append(
                FileResult(
                    path=result.path,
                    status=FileStatus.DROPPED,
                    message=message,
                    timings=result.timings,
                )
            )
        else:
            marked.append(result)
    return tuple(marked)


def _finish_timings(
    timings: dict[str, float],
    total_start: float,
    *,
    transaction: object | None = None,
) -> dict[str, float]:
    result = {
        **timings,
        "occ.commit.total_s": time.perf_counter() - total_start,
    }
    if transaction is not None:
        lock_held_s = getattr(transaction, "lock_held_s", None)
        if lock_held_s is not None:
            result["layer_stack.transaction.lock_held_s"] = float(lock_held_s)
    return result


__all__ = ["OccCommitTransaction"]
