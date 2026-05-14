"""Direct-path layer staging for gitignored and untracked changes."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import cast

from sandbox.layer_stack.layer.change import (
    DeleteLayerChange,
    LayerChange,
    LayerDelta,
    OpaqueDirLayerChange,
    SymlinkLayerChange,
)
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import PreparedPathGroup
from sandbox.occ.changeset.types import (
    Change,
    DeleteChange,
    EditChange,
    FileResult,
    FileStatus,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.occ.merge.policy import FinalKind, StageWrite, StageWriteFromPath
from sandbox.occ.ports import SnapshotReader
from sandbox.timing import monotonic_now

_DirectChangeHandler = Callable[[Change, "_DirectStageState"], FileResult | None]


@dataclass
class _DirectStageState:
    content: bytes
    initial_exists: bool
    final_kind: FinalKind
    symlink_target: str | None = None
    final_content_path: str | None = None
    final_precomputed_hash: str | None = None

    @classmethod
    def from_snapshot(
        cls,
        current_content: bytes | None,
        *,
        current_exists: bool,
    ) -> "_DirectStageState":
        return cls(
            content=current_content or b"",
            initial_exists=current_exists,
            final_kind="write" if current_exists else "delete",
        )

    def set_special(self, kind: FinalKind, *, symlink_target: str | None = None) -> None:
        self.content = b""
        self.final_kind = kind
        self.symlink_target = symlink_target
        self.final_content_path = None
        self.final_precomputed_hash = None

    def set_write(
        self,
        content: bytes,
        *,
        content_path: str | None,
        precomputed_hash: str | None,
    ) -> None:
        self.content = content
        self.final_kind = "write"
        self.symlink_target = None
        self.final_content_path = content_path
        self.final_precomputed_hash = precomputed_hash

    def set_delete(self) -> None:
        self.set_special("delete")


class DirectMerge:
    """Stage direct changes with last-writer-wins semantics."""

    def __init__(self, snapshot_reader: SnapshotReader) -> None:
        self._snapshot_reader = snapshot_reader
        self._handlers: dict[type[Change], _DirectChangeHandler] = {
            OpaqueDirChange: self._apply_opaque_dir,
            SymlinkChange: self._apply_symlink,
            WriteChange: self._apply_write,
            DeleteChange: self._apply_delete,
            EditChange: self._apply_edit,
        }

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
        read_start = monotonic_now()
        current_content, current_exists = self._snapshot_reader.read_bytes(
            group.path,
            active_manifest,
        )
        timings["occ.direct.read_current_s"] = monotonic_now() - read_start
        state = _DirectStageState.from_snapshot(
            current_content,
            current_exists=current_exists,
        )

        apply_start = monotonic_now()
        for change in group.changes:
            result = self._apply_change(change, state, path=group.path)
            if result is not None:
                timings["occ.direct.apply_changes_s"] = (
                    monotonic_now() - apply_start
                )
                return _with_timings(result, timings), None

        timings["occ.direct.apply_changes_s"] = monotonic_now() - apply_start
        stage_start = monotonic_now()
        if state.final_kind == "opaque_dir":
            timings["occ.direct.stage_delta_s"] = monotonic_now() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                LayerDelta(changes=(OpaqueDirLayerChange(path=group.path),)),
            )
        if state.final_kind == "symlink" and state.symlink_target is not None:
            delta = LayerDelta(
                changes=(
                    SymlinkLayerChange(
                        path=group.path,
                        source_path=state.symlink_target,
                    ),
                )
            )
            timings["occ.direct.stage_delta_s"] = monotonic_now() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                delta,
            )
        if state.final_kind == "write":
            if (
                stage_write_from_path is not None
                and state.final_content_path is not None
                and state.final_precomputed_hash is not None
            ):
                # Pass the already-loaded bytes through; stager's
                # small-file path skips the second disk read.
                delta = LayerDelta(
                    changes=(
                        stage_write_from_path(
                            group.path,
                            state.final_content_path,
                            state.final_precomputed_hash,
                            state.content,
                        ),
                    )
                )
            else:
                delta = LayerDelta(
                    changes=(stage_write(group.path, state.content),)
                )
            timings["occ.direct.stage_delta_s"] = monotonic_now() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                delta,
            )
        if state.final_kind == "delete" and state.initial_exists:
            timings["occ.direct.stage_delta_s"] = monotonic_now() - stage_start
            return (
                FileResult(
                    path=group.path,
                    status=FileStatus.ACCEPTED,
                    timings=timings,
                ),
                LayerDelta(changes=(DeleteLayerChange(path=group.path),)),
            )
        timings["occ.direct.stage_delta_s"] = monotonic_now() - stage_start
        return (
            FileResult(
                path=group.path,
                status=FileStatus.ACCEPTED,
                timings=timings,
            ),
            None,
        )

    def _apply_change(
        self,
        change: Change,
        state: _DirectStageState,
        *,
        path: str,
    ) -> FileResult | None:
        handler = self._handlers.get(type(change))
        if handler is None:
            return FileResult(
                path=path,
                status=FileStatus.REJECTED,
                message=f"unsupported direct change kind: {type(change).__name__}",
            )
        return handler(change, state)

    def _apply_opaque_dir(
        self,
        change: Change,
        state: _DirectStageState,
    ) -> FileResult | None:
        del change
        state.set_special("opaque_dir")
        return None

    def _apply_symlink(
        self,
        change: Change,
        state: _DirectStageState,
    ) -> FileResult | None:
        symlink = cast(SymlinkChange, change)
        state.set_special("symlink", symlink_target=symlink.target)
        return None

    def _apply_write(
        self,
        change: Change,
        state: _DirectStageState,
    ) -> FileResult | None:
        write = cast(WriteChange, change)
        state.set_write(
            bytes(write.final_content),
            content_path=write.content_path,
            precomputed_hash=write.precomputed_hash,
        )
        return None

    def _apply_delete(
        self,
        change: Change,
        state: _DirectStageState,
    ) -> FileResult | None:
        del change
        state.set_delete()
        return None

    def _apply_edit(
        self,
        change: Change,
        state: _DirectStageState,
    ) -> FileResult | None:
        edit = cast(EditChange, change)
        if state.final_kind != "write":
            return FileResult(
                path=edit.path,
                status=FileStatus.ABORTED_OVERLAP,
                message="file does not exist",
            )
        try:
            text = state.content.decode("utf-8")
        except UnicodeDecodeError:
            return FileResult(
                path=edit.path,
                status=FileStatus.ABORTED_OVERLAP,
                message="file is not utf-8 text",
            )
        count = text.count(edit.old_text)
        if count == 0:
            return FileResult(
                path=edit.path,
                status=FileStatus.ABORTED_OVERLAP,
                message="anchor not found",
            )
        if count != edit.expected_occurrences:
            return FileResult(
                path=edit.path,
                status=FileStatus.ABORTED_OVERLAP,
                message="anchor occurrence count mismatch",
            )
        text = text.replace(edit.old_text, edit.new_text, edit.expected_occurrences)
        state.set_write(
            text.encode("utf-8"),
            content_path=None,
            precomputed_hash=None,
        )
        return None


def _with_timings(result: FileResult, timings: dict[str, float]) -> FileResult:
    return FileResult(
        path=result.path,
        status=result.status,
        message=result.message,
        timings={**result.timings, **timings},
    )


__all__ = ["DirectMerge"]
