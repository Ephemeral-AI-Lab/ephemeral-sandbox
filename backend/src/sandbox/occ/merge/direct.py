"""Direct-path layer staging for gitignored and untracked changes."""

from __future__ import annotations

import time
from collections.abc import Callable
from typing import Literal

from sandbox.layer_stack.layer.change import LayerChange, LayerDelta
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import PreparedPathGroup
from sandbox.occ.changeset.types import (
    DeleteChange,
    EditChange,
    FileResult,
    FileStatus,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.occ.ports import SnapshotReader

StageWrite = Callable[[str, bytes], LayerChange]
StageWriteFromPath = Callable[[str, str, str, bytes | None], LayerChange]
_FinalKind = Literal["write", "delete", "symlink", "opaque_dir"]


class DirectMerge:
    """Stage direct changes with last-writer-wins semantics."""

    def __init__(self, snapshot_reader: SnapshotReader) -> None:
        self._snapshot_reader = snapshot_reader

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
        timings["occ.direct.read_current_s"] = time.perf_counter() - read_start
        initial_exists = current_exists
        content = current_content or b""
        final_kind: _FinalKind = "write" if current_exists else "delete"
        symlink_target: str | None = None
        # Track the final WriteChange's on-disk content_path so the
        # stager can copy from disk instead of round-tripping bytes
        # through Python.
        final_content_path: str | None = None
        final_precomputed_hash: str | None = None

        apply_start = time.perf_counter()
        for change in group.changes:
            if isinstance(change, OpaqueDirChange):
                content = b""
                final_kind = "opaque_dir"
                symlink_target = None
                final_content_path = None
                final_precomputed_hash = None
                continue
            if isinstance(change, SymlinkChange):
                symlink_target = change.target
                content = b""
                final_kind = "symlink"
                final_content_path = None
                final_precomputed_hash = None
                continue
            if isinstance(change, WriteChange):
                content = bytes(change.final_content)
                final_kind = "write"
                symlink_target = None
                final_content_path = change.content_path
                final_precomputed_hash = change.precomputed_hash
                continue
            if isinstance(change, DeleteChange):
                content = b""
                final_kind = "delete"
                symlink_target = None
                final_content_path = None
                final_precomputed_hash = None
                continue
            if isinstance(change, EditChange):
                # Loud rejection on every failure case, mirroring
                # GatedMerge._apply_edit_content. The previous silent
                # `continue` on missing anchor, count mismatch, non-utf-8
                # bytes, or prior-delete was the BL-01 contract violation —
                # gitignored paths could pocket bogus edits while
                # tracked paths got rejected.
                if final_kind != "write":
                    timings["occ.direct.apply_changes_s"] = (
                        time.perf_counter() - apply_start
                    )
                    return (
                        FileResult(
                            path=group.path,
                            status=FileStatus.ABORTED_OVERLAP,
                            message="file does not exist",
                            timings=timings,
                        ),
                        None,
                    )
                try:
                    text = content.decode("utf-8")
                except UnicodeDecodeError:
                    timings["occ.direct.apply_changes_s"] = (
                        time.perf_counter() - apply_start
                    )
                    return (
                        FileResult(
                            path=group.path,
                            status=FileStatus.ABORTED_OVERLAP,
                            message="file is not utf-8 text",
                            timings=timings,
                        ),
                        None,
                    )
                count = text.count(change.old_text)
                if count == 0:
                    timings["occ.direct.apply_changes_s"] = (
                        time.perf_counter() - apply_start
                    )
                    return (
                        FileResult(
                            path=group.path,
                            status=FileStatus.ABORTED_OVERLAP,
                            message="anchor not found",
                            timings=timings,
                        ),
                        None,
                    )
                if count != change.expected_occurrences:
                    timings["occ.direct.apply_changes_s"] = (
                        time.perf_counter() - apply_start
                    )
                    return (
                        FileResult(
                            path=group.path,
                            status=FileStatus.ABORTED_OVERLAP,
                            message="anchor occurrence count mismatch",
                            timings=timings,
                        ),
                        None,
                    )
                text = text.replace(
                    change.old_text,
                    change.new_text,
                    change.expected_occurrences,
                )
                content = text.encode("utf-8")
                final_content_path = None
                final_precomputed_hash = None
                continue

            timings["occ.direct.apply_changes_s"] = time.perf_counter() - apply_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.REJECTED,
                    message=f"unsupported direct change kind: {type(change).__name__}",
                    timings=timings,
                ),
                None,
            )

        timings["occ.direct.apply_changes_s"] = time.perf_counter() - apply_start
        stage_start = time.perf_counter()
        if final_kind == "opaque_dir":
            timings["occ.direct.stage_delta_s"] = time.perf_counter() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                LayerDelta(changes=(LayerChange(path=group.path, kind="opaque_dir"),)),
            )
        if final_kind == "symlink" and symlink_target is not None:
            delta = LayerDelta(
                changes=(
                    LayerChange(
                        path=group.path,
                        kind="symlink",
                        source_path=symlink_target,
                    ),
                )
            )
            timings["occ.direct.stage_delta_s"] = time.perf_counter() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                delta,
            )
        if final_kind == "write":
            if (
                stage_write_from_path is not None
                and final_content_path is not None
                and final_precomputed_hash is not None
            ):
                # Pass the already-loaded bytes through; stager's
                # small-file path skips the second disk read.
                delta = LayerDelta(
                    changes=(
                        stage_write_from_path(
                            group.path,
                            final_content_path,
                            final_precomputed_hash,
                            content,
                        ),
                    )
                )
            else:
                delta = LayerDelta(changes=(stage_write(group.path, content),))
            timings["occ.direct.stage_delta_s"] = time.perf_counter() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                delta,
            )
        if final_kind == "delete" and initial_exists:
            timings["occ.direct.stage_delta_s"] = time.perf_counter() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                LayerDelta(changes=(LayerChange(path=group.path, kind="delete"),)),
            )
        timings["occ.direct.stage_delta_s"] = time.perf_counter() - stage_start
        return (
            FileResult(
                path=group.path,
                status=FileStatus.ACCEPTED,
                timings=timings,
            ),
            None,
        )


__all__ = ["DirectMerge"]
