"""Shared staging policy contracts for OCC commits."""

from __future__ import annotations

from collections.abc import Callable
from typing import Literal, Protocol

from sandbox.layer_stack.layer_change import LayerChange, LayerDelta
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import PreparedPathGroup
from sandbox.occ.changeset.types import FileResult

StageWrite = Callable[[str, bytes], LayerChange]
StageWriteFromPath = Callable[[str, str, str, bytes | None], LayerChange]
FinalKind = Literal["write", "delete", "symlink", "opaque_dir"]


class MergePolicy(Protocol):
    """Validate and stage one prepared path group."""

    def stage_group(
        self,
        group: PreparedPathGroup,
        *,
        active_manifest: Manifest,
        stage_write: StageWrite,
        stage_write_from_path: StageWriteFromPath | None = None,
    ) -> tuple[FileResult, LayerDelta | None]: ...


def with_timings(result: FileResult, timings: dict[str, float]) -> FileResult:
    return FileResult(
        path=result.path,
        status=result.status,
        message=result.message,
        timings={**result.timings, **timings},
    )
