"""Atomic OCC validation and layer publish transaction."""

from __future__ import annotations

import tempfile
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.changes import LayerChange, LayerDelta
from sandbox.layer_stack.manifest import STAGING_DIR
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.intent import (
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
)
from sandbox.occ.changeset.types import (
    ChangesetResult,
    FileResult,
    FileStatus,
)
from sandbox.occ.direct.merge import DirectMerge
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.gated.merge import GatedMerge


@dataclass(frozen=True)
class PathValidation:
    path: str
    result: FileResult
    accepted_delta: LayerDelta | None


class OccCommitTransaction:
    """Revalidate prepared OCC path groups and publish one immutable layer."""

    def __init__(self, layer_stack: LayerStackManager) -> None:
        self._layer_stack = layer_stack
        self._hasher = ContentHasher()
        self._gated = GatedMerge(layer_stack, hasher=self._hasher)
        self._direct = DirectMerge(layer_stack)

    def revalidate_and_publish(self, prepared: PreparedChangeset) -> ChangesetResult:
        """Validate against the current active manifest and publish accepted deltas."""
        with self._layer_stack.commit_transaction() as transaction:
            active_manifest = transaction.snapshot()
            with _LayerChangeStager(
                self._layer_stack.storage_root,
                hasher=self._hasher,
            ) as stager:
                validations: list[PathValidation] = []
                tracked_failed = False
                for group in prepared.path_groups:
                    validation = self._validate_group(
                        group,
                        active_manifest=active_manifest,
                        stager=stager,
                    )
                    validations.append(validation)
                    if (
                        group.route is RouteDecision.TRACKED
                        and validation.result.status is not FileStatus.ACCEPTED
                    ):
                        tracked_failed = True

                files = tuple(validation.result for validation in validations)
                if _must_skip_publish(
                    prepared,
                    files,
                    tracked_failed=tracked_failed,
                ):
                    return ChangesetResult(
                        files=tuple(_mark_unpublished(files, prepared)),
                        published_manifest_version=None,
                    )

                changes = tuple(
                    change
                    for validation in validations
                    if validation.accepted_delta is not None
                    for change in validation.accepted_delta.changes
                )
                if not changes:
                    return ChangesetResult(files=files, published_manifest_version=None)

                published = transaction.publish_layer(changes)
                return ChangesetResult(
                    files=files,
                    published_manifest_version=published.version,
                )

    def _validate_group(
        self,
        group: PreparedPathGroup,
        *,
        active_manifest,
        stager: "_LayerChangeStager",
    ) -> PathValidation:
        if group.route is RouteDecision.DROP:
            return PathValidation(
                path=group.path,
                result=FileResult(
                    path=group.path,
                    status=FileStatus.DROPPED,
                    message=group.message or "change dropped",
                ),
                accepted_delta=None,
            )
        if group.route is RouteDecision.REJECT:
            return PathValidation(
                path=group.path,
                result=FileResult(
                    path=group.path,
                    status=FileStatus.REJECTED,
                    message=group.message or "change rejected",
                ),
                accepted_delta=None,
            )
        if group.route is RouteDecision.DIRECT:
            result, delta = self._direct.stage_group(
                group,
                active_manifest=active_manifest,
                stage_write=stager.write,
            )
            return PathValidation(path=group.path, result=result, accepted_delta=delta)
        if group.route is RouteDecision.TRACKED:
            result, delta = self._gated.stage_group(
                group,
                active_manifest=active_manifest,
                stage_write=stager.write,
            )
            return PathValidation(path=group.path, result=result, accepted_delta=delta)
        return PathValidation(
            path=group.path,
            result=FileResult(
                path=group.path,
                status=FileStatus.REJECTED,
                message=f"unsupported route: {group.route}",
            ),
            accepted_delta=None,
        )


class _LayerChangeStager:
    def __init__(self, storage_root: Path, *, hasher: ContentHasher) -> None:
        self._staging_parent = storage_root / STAGING_DIR
        self._staging_parent.mkdir(parents=True, exist_ok=True)
        self._hasher = hasher
        self._counter = 0
        self._tmp: tempfile.TemporaryDirectory[str] | None = None

    def __enter__(self) -> "_LayerChangeStager":
        self._tmp = tempfile.TemporaryDirectory(
            prefix="occ-commit-",
            dir=str(self._staging_parent),
        )
        return self

    def __exit__(self, exc_type, exc, traceback) -> None:
        del exc_type, exc, traceback
        if self._tmp is not None:
            self._tmp.cleanup()
            self._tmp = None

    def write(self, path: str, content: bytes) -> LayerChange:
        if self._tmp is None:
            raise RuntimeError("OCC layer-change stager is not active")
        self._counter += 1
        source = Path(self._tmp.name) / f"{self._counter:06d}.bin"
        source.write_bytes(content)
        return LayerChange(
            path=path,
            kind="write",
            content_hash=self._hasher.hash_bytes(content),
            source_path=str(source),
        )


def _must_skip_publish(
    prepared: PreparedChangeset,
    files: tuple[FileResult, ...],
    *,
    tracked_failed: bool,
) -> bool:
    if prepared.atomic and any(_is_failure(result) for result in files):
        return True
    return _is_shell_changeset(prepared) and tracked_failed


def _is_shell_changeset(prepared: PreparedChangeset) -> bool:
    return any(
        change.source == "shell_capture"
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
        message = "not published because shell tracked validation failed"

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


__all__ = ["OccCommitTransaction", "PathValidation"]
