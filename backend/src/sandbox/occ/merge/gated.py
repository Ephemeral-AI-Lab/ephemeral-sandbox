"""Tracked-path OCC merge validation."""

from __future__ import annotations

import time
from collections.abc import Callable

from sandbox.layer_stack.layer.change import LayerChange, LayerDelta
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import PreparedPathGroup
from sandbox.occ.changeset.types import (
    DeleteChange,
    EditChange,
    FileResult,
    FileStatus,
    WriteChange,
)
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.ports import SnapshotReader

StageWrite = Callable[[str, bytes], LayerChange]
StageWriteFromPath = Callable[[str, str, str, bytes | None], LayerChange]


class GatedMerge:
    """Validate gated changes against the active manifest and stage a delta."""

    def __init__(
        self,
        snapshot_reader: SnapshotReader,
        *,
        hasher: ContentHasher | None = None,
    ) -> None:
        self._snapshot_reader = snapshot_reader
        self._hasher = hasher or ContentHasher()

    def stage_group(
        self,
        group: PreparedPathGroup,
        *,
        active_manifest: Manifest,
        stage_write: StageWrite,
        stage_write_from_path: StageWriteFromPath | None = None,
    ) -> tuple[FileResult, LayerDelta | None]:
        try:
            return self._stage_group(
                group,
                active_manifest,
                stage_write,
                stage_write_from_path,
            )
        except Exception as exc:
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.FAILED,
                    message=str(exc),
                ),
                None,
            )

    def _stage_group(
        self,
        group: PreparedPathGroup,
        active_manifest: Manifest,
        stage_write: StageWrite,
        stage_write_from_path: StageWriteFromPath | None,
    ) -> tuple[FileResult, LayerDelta | None]:
        timings: dict[str, float] = {}
        read_start = time.perf_counter()
        current_content, current_exists = self._snapshot_reader.read_bytes(
            group.path,
            active_manifest,
        )
        timings["occ.gated.read_current_s"] = time.perf_counter() - read_start
        initial_exists = current_exists
        content = current_content or b""
        exists = current_exists
        # Track the staging hint for the *final* WriteChange in the
        # group so the stager can copy from disk instead of round-
        # tripping bytes through Python. Reset on any non-WriteChange
        # so a chained Edit/Delete can't stage a stale path/hash.
        final_content_path: str | None = None
        final_precomputed_hash: str | None = None

        apply_start = time.perf_counter()
        for change in group.changes:
            current_hash = self._hasher.hash_current(content, exists=exists)
            if isinstance(change, WriteChange):
                expected_hash = _base_hash(change.base_hash)
                if current_hash != expected_hash:
                    timings["occ.gated.apply_changes_s"] = (
                        time.perf_counter() - apply_start
                    )
                    return (
                        FileResult(
                            path=group.path,
                            status=FileStatus.ABORTED_VERSION,
                            message="content changed",
                            timings=timings,
                        ),
                        None,
                    )
                # Eager bytes path for api_write/api_edit; lazy for
                # overlay capture (final_content reads from content_path
                # on demand).
                content = bytes(change.final_content)
                exists = True
                final_content_path = change.content_path
                final_precomputed_hash = change.precomputed_hash
                continue

            if isinstance(change, DeleteChange):
                expected_hash = _base_hash(change.base_hash)
                if current_hash != expected_hash:
                    timings["occ.gated.apply_changes_s"] = (
                        time.perf_counter() - apply_start
                    )
                    return (
                        FileResult(
                            path=group.path,
                            status=FileStatus.ABORTED_VERSION,
                            message="content changed before delete",
                            timings=timings,
                        ),
                        None,
                    )
                content = b""
                exists = False
                final_content_path = None
                final_precomputed_hash = None
                continue

            if isinstance(change, EditChange):
                edit_result = _apply_edit_content(
                    group.path,
                    content,
                    exists,
                    change,
                )
                if isinstance(edit_result, FileResult):
                    timings["occ.gated.apply_changes_s"] = (
                        time.perf_counter() - apply_start
                    )
                    return _with_timings(edit_result, timings), None
                content = edit_result
                exists = True
                # Edit produced new bytes — disk-backed shortcut no
                # longer represents the final content.
                final_content_path = None
                final_precomputed_hash = None
                continue

            timings["occ.gated.apply_changes_s"] = time.perf_counter() - apply_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.REJECTED,
                    message=f"unsupported tracked change kind: {type(change).__name__}",
                    timings=timings,
                ),
                None,
            )

        timings["occ.gated.apply_changes_s"] = time.perf_counter() - apply_start
        stage_start = time.perf_counter()
        delta = _delta_for_final_state(
            path=group.path,
            content=content,
            exists=exists,
            initial_exists=initial_exists,
            stage_write=stage_write,
            stage_write_from_path=stage_write_from_path,
            content_path=final_content_path,
            precomputed_hash=final_precomputed_hash,
        )
        timings["occ.gated.stage_delta_s"] = time.perf_counter() - stage_start
        return (
            FileResult(
                path=group.path,
                status=FileStatus.ACCEPTED,
                timings=timings,
            ),
            delta,
        )


def _base_hash(value: str | None) -> str | None:
    return value or None


def _apply_edit_content(
    path: str,
    content: bytes,
    exists: bool,
    change: EditChange,
) -> bytes | FileResult:
    if not exists:
        return FileResult(
            path=path,
            status=FileStatus.ABORTED_OVERLAP,
            message="file does not exist",
        )
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError:
        return FileResult(
            path=path,
            status=FileStatus.ABORTED_OVERLAP,
            message="file is not utf-8 text",
        )
    count = text.count(change.old_text)
    if count == 0:
        return FileResult(
            path=path,
            status=FileStatus.ABORTED_OVERLAP,
            message="anchor not found",
        )
    if count != change.expected_occurrences:
        return FileResult(
            path=path,
            status=FileStatus.ABORTED_OVERLAP,
            message="anchor occurrence count mismatch",
        )
    text = text.replace(change.old_text, change.new_text, change.expected_occurrences)
    return text.encode("utf-8")


def _delta_for_final_state(
    *,
    path: str,
    content: bytes,
    exists: bool,
    initial_exists: bool,
    stage_write: StageWrite,
    stage_write_from_path: StageWriteFromPath | None = None,
    content_path: str | None = None,
    precomputed_hash: str | None = None,
) -> LayerDelta | None:
    if exists:
        if (
            stage_write_from_path is not None
            and content_path is not None
            and precomputed_hash is not None
        ):
            # `content` was already materialised in the apply loop for
            # the hash chain — pass it through so the stager's small-
            # file path doesn't re-read content_path.
            return LayerDelta(
                changes=(
                    stage_write_from_path(
                        path, content_path, precomputed_hash, content
                    ),
                )
            )
        return LayerDelta(changes=(stage_write(path, content),))
    if initial_exists:
        return LayerDelta(changes=(LayerChange(path=path, kind="delete"),))
    return None


def _with_timings(result: FileResult, timings: dict[str, float]) -> FileResult:
    return FileResult(
        path=result.path,
        status=result.status,
        message=result.message,
        timings={**result.timings, **timings},
    )


__all__ = ["GatedMerge"]
