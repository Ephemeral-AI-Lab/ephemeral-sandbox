"""Apply snapshot overlay captures through OCC."""

from __future__ import annotations

import os
import time
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Protocol

from sandbox.occ.changeset.builders import (
    build_overlay_delete_change,
    build_overlay_write_change,
)
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import (
    Change,
    ChangesetResult,
    OpaqueDirChange,
    SymlinkChange,
)
from sandbox.occ.service import OccService
from sandbox.overlay.capture.changes import OverlayPathChange
from sandbox.overlay.capture.types import OverlayCapture, read_output_ref
from sandbox.overlay.runner.snapshot_overlay_runner import (
    OverlayShellRequest,
    SnapshotOverlayRunner,
)


@dataclass(frozen=True)
class OverlayShellCommitResult:
    capture: OverlayCapture
    changeset: ChangesetResult
    stdout: str
    stderr: str
    timings: dict[str, float]


class OccApplyClient(Protocol):
    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        agent_id: str = "",
        description: str = "",
        snapshot: object | None = None,
    ) -> ChangesetResult | PreparedChangeset: ...


def run_overlay_shell_commit(
    *,
    runner: SnapshotOverlayRunner,
    occ_service: OccService,
    request: OverlayShellRequest,
    agent_id: str = "",
    description: str = "",
) -> OverlayShellCommitResult:
    """Run overlay shell capture and commit that capture through OCC."""
    total_start = time.perf_counter()
    overlay_start = time.perf_counter()
    capture = runner.shell_sync(request)
    overlay_elapsed = time.perf_counter() - overlay_start

    occ_start = time.perf_counter()
    changeset = apply_overlay_capture_sync(
        capture,
        occ_service=occ_service,
        agent_id=agent_id,
        description=description,
    )
    occ_elapsed = time.perf_counter() - occ_start

    worker_elapsed = time.perf_counter() - total_start
    timings = {
        **capture.timings,
        **changeset.timings,
        "api.shell.overlay_s": overlay_elapsed,
        "api.shell.occ_apply_s": occ_elapsed,
        "api.shell.worker_total_s": worker_elapsed,
        "api.shell.total_s": worker_elapsed,
    }
    return OverlayShellCommitResult(
        capture=capture,
        changeset=changeset,
        stdout=read_output_ref(capture.stdout_ref),
        stderr=read_output_ref(capture.stderr_ref),
        timings=timings,
    )


def overlay_capture_to_occ_changes(capture: OverlayCapture) -> tuple[Change, ...]:
    """Convert policy-blind overlay path changes into source-tagged OCC changes."""
    return overlay_path_changes_to_occ_changes(capture.changes)


def overlay_path_changes_to_occ_changes(
    path_changes: Sequence[OverlayPathChange],
) -> tuple[Change, ...]:
    changes: list[Change] = []
    for path_change in path_changes:
        if path_change.kind == "write":
            if path_change.content_path is None:
                raise ValueError(
                    f"write overlay path change lacks content path: {path_change.path}"
                )
            if path_change.final_hash is None:
                raise ValueError(
                    f"write overlay path change lacks final_hash: {path_change.path}"
                )
            # Phase 3 improvement #2: thread content_path + precomputed
            # hash through; stager copies in-kernel.
            changes.append(
                build_overlay_write_change(
                    path=path_change.path,
                    content_path=path_change.content_path,
                    precomputed_hash=path_change.final_hash,
                )
            )
            continue
        if path_change.kind == "delete":
            changes.append(build_overlay_delete_change(path=path_change.path))
            continue
        if path_change.kind == "symlink":
            if path_change.content_path is None:
                raise ValueError(
                    f"symlink overlay path change lacks content path: {path_change.path}"
                )
            changes.append(
                SymlinkChange(
                    path=path_change.path,
                    target=os.readlink(path_change.content_path),
                    source="overlay_capture",
                )
            )
            continue
        if path_change.kind == "opaque_dir":
            changes.append(
                OpaqueDirChange(
                    path=path_change.path,
                    kept_children=frozenset(
                        _kept_children_for(path_change.path, path_changes)
                    ),
                    source="overlay_capture",
                )
            )
            continue
    return tuple(changes)


async def apply_overlay_capture(
    capture: OverlayCapture,
    *,
    occ_client: OccApplyClient,
    agent_id: str = "",
    description: str = "",
) -> ChangesetResult:
    """Commit an overlay capture through OCC."""
    changes = overlay_capture_to_occ_changes(capture)
    if not changes:
        return ChangesetResult(
            files=(),
            timings=dict(capture.timings),
            published_manifest_version=None,
        )
    if capture.snapshot_manifest is None:
        raise ValueError("overlay capture is missing its leased manifest")

    result = await occ_client.apply_changeset(
        changes,
        agent_id=agent_id,
        description=description,
        snapshot=capture.snapshot_manifest,
    )
    if isinstance(result, PreparedChangeset):
        raise TypeError("overlay capture OCC service returned an uncommitted changeset")
    return ChangesetResult(
        files=result.files,
        timings={**capture.timings, **result.timings},
        published_manifest_version=result.published_manifest_version,
    )


def apply_overlay_capture_sync(
    capture: OverlayCapture,
    *,
    occ_service: OccService,
    agent_id: str,
    description: str,
) -> ChangesetResult:
    """Synchronously commit an overlay capture through OCC."""
    changes = overlay_capture_to_occ_changes(capture)
    if not changes:
        return ChangesetResult(
            files=(),
            timings=dict(capture.timings),
            published_manifest_version=None,
        )
    if capture.snapshot_manifest is None:
        raise ValueError("overlay capture is missing its leased manifest")
    result = occ_service.apply_changeset_sync(
        changes,
        snapshot=capture.snapshot_manifest,
        options=CommitOptions(caller_id=agent_id, description=description),
    )
    if isinstance(result, PreparedChangeset):
        raise TypeError("overlay capture OCC service returned an uncommitted changeset")
    return result


def _kept_children_for(
    rel: str,
    path_changes: Sequence[OverlayPathChange],
) -> set[str]:
    prefix = f"{rel}/" if rel else ""
    kept: set[str] = set()
    for item in path_changes:
        if item.path == rel or not item.path.startswith(prefix):
            continue
        rest = item.path[len(prefix) :]
        if rest:
            kept.add(rest.split("/", 1)[0])
    return kept


__all__ = [
    "OccApplyClient",
    "OverlayShellCommitResult",
    "apply_overlay_capture",
    "apply_overlay_capture_sync",
    "overlay_capture_to_occ_changes",
    "overlay_path_changes_to_occ_changes",
    "run_overlay_shell_commit",
]
