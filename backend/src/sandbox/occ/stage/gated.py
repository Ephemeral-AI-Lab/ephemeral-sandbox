"""Tracked-path OCC merge validation."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import cast

from sandbox.layer_stack.layer_change import (
    DeleteLayerChange,
    LayerChange,
    LayerDelta,
    OpaqueDirLayerChange,
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
    WriteChange,
)
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.stage._edit import apply_edit_content
from sandbox.occ.stage.policy import StageWrite, StageWriteFromPath, with_timings
from sandbox.occ.ports import SnapshotReader
from sandbox.occ.timing_keys import TimingKey
from sandbox.timing import monotonic_now

_GatedChangeHandler = Callable[
    [Change, "_GatedStageState", str | None, str],
    FileResult | None,
]


@dataclass
class _GatedStageState:
    content: bytes
    exists: bool
    initial_exists: bool
    final_content_path: str | None = None
    final_precomputed_hash: str | None = None
    final_special_change: LayerChange | None = None

    @classmethod
    def from_snapshot(
        cls,
        current_content: bytes | None,
        *,
        current_exists: bool,
    ) -> "_GatedStageState":
        return cls(
            content=current_content or b"",
            exists=current_exists,
            initial_exists=current_exists,
        )

    def set_opaque_dir(self, *, path: str) -> None:
        self.content = b""
        self.exists = True
        self.final_content_path = None
        self.final_precomputed_hash = None
        self.final_special_change = OpaqueDirLayerChange(path=path)

    def set_write(
        self,
        content: bytes,
        *,
        content_path: str | None,
        precomputed_hash: str | None,
    ) -> None:
        self.content = content
        self.exists = True
        self.final_content_path = content_path
        self.final_precomputed_hash = precomputed_hash
        self.final_special_change = None

    def set_delete(self) -> None:
        self.content = b""
        self.exists = False
        self.final_content_path = None
        self.final_precomputed_hash = None
        self.final_special_change = None


class GatedStager:
    """Validate gated changes against the active manifest and stage a delta."""

    def __init__(
        self,
        snapshot_reader: SnapshotReader,
        *,
        hasher: ContentHasher | None = None,
    ) -> None:
        self._snapshot_reader = snapshot_reader
        self._hasher = hasher or ContentHasher()
        self._handlers: dict[type[Change], _GatedChangeHandler] = {
            OpaqueDirChange: self._apply_opaque_dir,
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
        timings[TimingKey.GATED_READ_CURRENT] = monotonic_now() - read_start
        state = _GatedStageState.from_snapshot(
            current_content,
            current_exists=current_exists,
        )

        apply_start = monotonic_now()
        for change in group.changes:
            current_hash = self._hasher.hash_current(
                state.content,
                exists=state.exists,
            )
            result = self._apply_change(
                change,
                state,
                current_hash=current_hash,
                path=group.path,
            )
            if result is not None:
                timings[TimingKey.GATED_APPLY_CHANGES] = monotonic_now() - apply_start
                return with_timings(result, timings), None

        timings[TimingKey.GATED_APPLY_CHANGES] = monotonic_now() - apply_start
        stage_start = monotonic_now()
        delta = (
            LayerDelta(changes=(state.final_special_change,))
            if state.final_special_change is not None
            else _delta_for_final_state(
                path=group.path,
                content=state.content,
                exists=state.exists,
                initial_exists=state.initial_exists,
                stage_write=stage_write,
                stage_write_from_path=stage_write_from_path,
                content_path=state.final_content_path,
                precomputed_hash=state.final_precomputed_hash,
            )
        )
        timings[TimingKey.GATED_STAGE_DELTA] = monotonic_now() - stage_start
        return (
            FileResult(
                path=group.path,
                status=FileStatus.ACCEPTED,
                timings=timings,
            ),
            delta,
        )

    def _apply_change(
        self,
        change: Change,
        state: _GatedStageState,
        *,
        current_hash: str | None,
        path: str,
    ) -> FileResult | None:
        handler = self._handlers.get(type(change))
        if handler is None:
            return FileResult(
                path=path,
                status=FileStatus.REJECTED,
                message=f"unsupported tracked change kind: {type(change).__name__}",
            )
        return handler(change, state, current_hash, path)

    def _apply_opaque_dir(
        self,
        change: Change,
        state: _GatedStageState,
        current_hash: str | None,
        path: str,
    ) -> FileResult | None:
        del change, current_hash
        state.set_opaque_dir(path=path)
        return None

    def _apply_write(
        self,
        change: Change,
        state: _GatedStageState,
        current_hash: str | None,
        path: str,
    ) -> FileResult | None:
        write = cast(WriteChange, change)
        expected_hash = _base_hash(write.base_hash)
        if current_hash != expected_hash:
            return FileResult(
                path=path,
                status=FileStatus.ABORTED_VERSION,
                message="content changed",
            )
        state.set_write(
            bytes(write.final_content),
            content_path=write.content_path,
            precomputed_hash=write.precomputed_hash,
        )
        return None

    def _apply_delete(
        self,
        change: Change,
        state: _GatedStageState,
        current_hash: str | None,
        path: str,
    ) -> FileResult | None:
        delete = cast(DeleteChange, change)
        expected_hash = _base_hash(delete.base_hash)
        if current_hash != expected_hash:
            return FileResult(
                path=path,
                status=FileStatus.ABORTED_VERSION,
                message="content changed before delete",
            )
        state.set_delete()
        return None

    def _apply_edit(
        self,
        change: Change,
        state: _GatedStageState,
        current_hash: str | None,
        path: str,
    ) -> FileResult | None:
        del current_hash
        edit = cast(EditChange, change)
        edit_result = apply_edit_content(
            path,
            state.content,
            state.exists,
            edit,
        )
        if isinstance(edit_result, FileResult):
            return edit_result
        state.set_write(
            edit_result,
            content_path=None,
            precomputed_hash=None,
        )
        return None


def _base_hash(value: str | None) -> str | None:
    return value or None


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
                changes=(stage_write_from_path(path, content_path, precomputed_hash, content),)
            )
        return LayerDelta(changes=(stage_write(path, content),))
    if initial_exists:
        return LayerDelta(changes=(DeleteLayerChange(path=path),))
    return None


__all__ = ["GatedStager"]
