"""Unified OCC stager parameterised by route.

The same validation+staging loop drives both routes; the only per-route
differences are (a) whether to enforce the base-hash predicate before
writes/deletes, (b) whether SymlinkChange is supported, and (c) the
status to return when an EditChange targets a missing file.
"""

from __future__ import annotations

from dataclasses import dataclass

from sandbox.layer_stack.changes import (
    DeleteLayerChange,
    OpaqueDirLayerChange,
    SymlinkLayerChange,
)
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset import (
    Change,
    DeleteChange,
    EditChange,
    FileResult,
    FileStatus,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
    PreparedPathGroup,
)
from sandbox.occ.hashing import ContentHasher
from sandbox.occ.protocols import SnapshotReader
from sandbox.occ.stage_policy import (
    FinalKind,
    StageWrite,
    StageWriteFromPath,
    StagedChanges,
    with_timings,
)
from sandbox.timing_keys import TimingKey
from sandbox._shared.clock import monotonic_now


def apply_edit_content(
    path: str,
    content: bytes,
    change: EditChange,
) -> bytes | FileResult:
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


@dataclass(frozen=True)
class _RouteProfile:
    """Route-specific configuration for the unified Stager."""

    name: str
    check_hash: bool
    supports_symlinks: bool
    missing_file_status: FileStatus
    timing_read: TimingKey
    timing_apply: TimingKey
    timing_stage: TimingKey


_DIRECT_PROFILE = _RouteProfile(
    name="direct",
    check_hash=False,
    supports_symlinks=True,
    missing_file_status=FileStatus.REJECTED,
    timing_read=TimingKey.DIRECT_READ_CURRENT,
    timing_apply=TimingKey.DIRECT_APPLY_CHANGES,
    timing_stage=TimingKey.DIRECT_STAGE_DELTA,
)

_GATED_PROFILE = _RouteProfile(
    name="tracked",
    check_hash=True,
    supports_symlinks=False,
    missing_file_status=FileStatus.ABORTED_VERSION,
    timing_read=TimingKey.GATED_READ_CURRENT,
    timing_apply=TimingKey.GATED_APPLY_CHANGES,
    timing_stage=TimingKey.GATED_STAGE_DELTA,
)


@dataclass
class _StageState:
    content: bytes
    initial_exists: bool
    final_kind: FinalKind
    symlink_target: str | None = None
    final_content_path: str | None = None
    final_precomputed_hash: str | None = None

    @property
    def exists(self) -> bool:
        return self.final_kind != "delete"


class Stager:
    """Validate and stage one prepared path group, parameterised by route."""

    def __init__(
        self,
        snapshot_reader: SnapshotReader,
        profile: _RouteProfile,
        *,
        hasher: ContentHasher | None = None,
    ) -> None:
        self._snapshot_reader = snapshot_reader
        self._profile = profile
        self._hasher = hasher

    @classmethod
    def direct(cls, snapshot_reader: SnapshotReader) -> Stager:
        return cls(snapshot_reader, _DIRECT_PROFILE)

    @classmethod
    def gated(
        cls,
        snapshot_reader: SnapshotReader,
        *,
        hasher: ContentHasher | None = None,
    ) -> Stager:
        return cls(snapshot_reader, _GATED_PROFILE, hasher=hasher or ContentHasher())

    def stage_group(
        self,
        group: PreparedPathGroup,
        *,
        active_manifest: Manifest,
        stage_write: StageWrite,
        stage_write_from_path: StageWriteFromPath | None = None,
    ) -> tuple[FileResult, StagedChanges | None]:
        try:
            return self._stage_group(group, active_manifest, stage_write, stage_write_from_path)
        except Exception as exc:
            return (
                FileResult(path=group.path, status=FileStatus.FAILED, message=str(exc)),
                None,
            )

    def _stage_group(
        self,
        group: PreparedPathGroup,
        active_manifest: Manifest,
        stage_write: StageWrite,
        stage_write_from_path: StageWriteFromPath | None,
    ) -> tuple[FileResult, StagedChanges | None]:
        profile = self._profile
        timings: dict[str, float] = {}

        read_start = monotonic_now()
        current_content, current_exists = self._snapshot_reader.read_bytes(
            group.path, active_manifest
        )
        timings[profile.timing_read] = monotonic_now() - read_start

        state = _StageState(
            content=current_content or b"",
            initial_exists=current_exists,
            final_kind="write" if current_exists else "delete",
        )

        apply_start = monotonic_now()
        for change in group.changes:
            result = self._apply_change(change, state, group.path)
            if result is not None:
                timings[profile.timing_apply] = monotonic_now() - apply_start
                return with_timings(result, timings), None
        timings[profile.timing_apply] = monotonic_now() - apply_start

        stage_start = monotonic_now()
        delta = self._build_delta(group.path, state, stage_write, stage_write_from_path)
        timings[profile.timing_stage] = monotonic_now() - stage_start

        return (
            FileResult(path=group.path, status=FileStatus.ACCEPTED, timings=timings),
            delta,
        )

    def _apply_change(
        self,
        change: Change,
        state: _StageState,
        path: str,
    ) -> FileResult | None:
        if isinstance(change, OpaqueDirChange):
            state.content = b""
            state.final_kind = "opaque_dir"
            state.symlink_target = None
            state.final_content_path = None
            state.final_precomputed_hash = None
            return None

        if isinstance(change, WriteChange):
            mismatch = self._hash_mismatch(state, change.base_hash)
            if mismatch is not None:
                return FileResult(path=path, status=mismatch, message="content changed")
            state.content = bytes(change.final_content)
            state.final_kind = "write"
            state.symlink_target = None
            state.final_content_path = change.content_path
            state.final_precomputed_hash = change.precomputed_hash
            return None

        if isinstance(change, DeleteChange):
            mismatch = self._hash_mismatch(state, change.base_hash)
            if mismatch is not None:
                return FileResult(
                    path=path,
                    status=mismatch,
                    message="content changed before delete",
                )
            state.content = b""
            state.final_kind = "delete"
            state.symlink_target = None
            state.final_content_path = None
            state.final_precomputed_hash = None
            return None

        if isinstance(change, EditChange):
            if state.final_kind != "write":
                return FileResult(
                    path=path,
                    status=self._profile.missing_file_status,
                    message="file does not exist",
                )
            edit_result = apply_edit_content(path, state.content, change)
            if isinstance(edit_result, FileResult):
                return edit_result
            state.content = edit_result
            state.final_kind = "write"
            state.symlink_target = None
            state.final_content_path = None
            state.final_precomputed_hash = None
            return None

        if isinstance(change, SymlinkChange):
            if not self._profile.supports_symlinks:
                return FileResult(
                    path=path,
                    status=FileStatus.REJECTED,
                    message=f"unsupported {self._profile.name} change kind: SymlinkChange",
                )
            state.content = b""
            state.final_kind = "symlink"
            state.symlink_target = change.target
            state.final_content_path = None
            state.final_precomputed_hash = None
            return None

        return FileResult(
            path=path,
            status=FileStatus.REJECTED,
            message=f"unsupported {self._profile.name} change kind: {type(change).__name__}",
        )

    def _hash_mismatch(self, state: _StageState, base_hash: str | None) -> FileStatus | None:
        """Return ABORTED_VERSION if the gated hash chain disagrees, else None."""
        if not self._profile.check_hash or self._hasher is None:
            return None
        expected = base_hash or None
        current = self._hasher.hash_current(state.content, exists=state.exists)
        if current != expected:
            return FileStatus.ABORTED_VERSION
        return None

    def _build_delta(
        self,
        path: str,
        state: _StageState,
        stage_write: StageWrite,
        stage_write_from_path: StageWriteFromPath | None,
    ) -> StagedChanges | None:
        if state.final_kind == "opaque_dir":
            return (OpaqueDirLayerChange(path=path),)
        if state.final_kind == "symlink" and state.symlink_target is not None:
            return (SymlinkLayerChange(path=path, source_path=state.symlink_target),)
        if state.final_kind == "write":
            if (
                stage_write_from_path is not None
                and state.final_content_path is not None
                and state.final_precomputed_hash is not None
            ):
                # Pass the in-memory bytes so the stager's small-file path
                # skips re-reading content_path from disk.
                return (
                    stage_write_from_path(
                        path,
                        state.final_content_path,
                        state.final_precomputed_hash,
                        state.content,
                    ),
                )
            return (stage_write(path, state.content),)
        if state.final_kind == "delete" and state.initial_exists:
            return (DeleteLayerChange(path=path),)
        return None


def DirectStager(snapshot_reader: SnapshotReader) -> Stager:
    """Stage direct (gitignored / untracked) changes with last-writer-wins."""
    return Stager.direct(snapshot_reader)


def GatedStager(
    snapshot_reader: SnapshotReader,
    *,
    hasher: ContentHasher | None = None,
) -> Stager:
    """Stage tracked changes, validating each step's base-hash chain."""
    return Stager.gated(snapshot_reader, hasher=hasher)


__all__ = ["DirectStager", "GatedStager", "Stager"]
