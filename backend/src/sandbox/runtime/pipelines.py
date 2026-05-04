"""In-sandbox runtime pipelines."""

from __future__ import annotations

import inspect
from collections.abc import Callable
from typing import Any, Protocol

from sandbox.occ.changeset.intent import PreparedChangeset
from sandbox.occ.changeset.types import (
    Change,
    ChangesetResult,
    DeleteChange,
    FileResult,
    FileStatus,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
    is_published_status,
    is_success_status,
)
from sandbox.runtime.overlay_capture import OverlayCaptureEngine, OverlayEngine
from sandbox.runtime.overlay_capture.types import OverlayRunOutcome
from sandbox.runtime.types import ConflictInfo, ShellResult


class _ChangesetApplier(Protocol):
    """Commit service subset used by ``shell_pipeline``."""

    async def apply_changeset(
        self,
        changes: list[Change],
    ) -> ChangesetResult | PreparedChangeset: ...


async def shell_pipeline(
    *,
    command: str,
    workspace_root: str = "/workspace",
    sandbox_id: str = "local",
    timeout: int | None = None,
    stdin: str | None = None,
    description: str = "",
    agent_id: str = "",
    overlay_engine: OverlayEngine | None = None,
    changeset_applier: _ChangesetApplier | None = None,
    overlay_sandbox: Any = None,
    on_progress_line: Callable[[str], None] | None = None,
) -> ShellResult:
    """Run shell through overlay capture, then project the gate's verdict.

    Overlay capture is policy-blind. Mutating commands require an explicit
    typed OCC changeset applier supplied by the layer-stack integration path.
    """
    owns_overlay = overlay_engine is None
    overlay = overlay_engine or OverlayCaptureEngine(
        sandbox_id=sandbox_id,
        workspace_root=workspace_root,
        direct_runtime=True,
    )
    try:
        outcome = await _execute_overlay(
            overlay,
            command,
            sandbox=overlay_sandbox,
            timeout=timeout,
            stdin=stdin,
            description=description,
            agent_id=agent_id,
            on_progress_line=on_progress_line,
        )

        typed_changes = _overlay_changes_to_changeset(outcome.upper_changes)
        result = await _apply_changeset(
            typed_changes,
            changeset_applier=changeset_applier,
        )
        return _shell_result_from_changeset(outcome, result, workspace_root=workspace_root)
    finally:
        if owns_overlay:
            dispose = getattr(overlay, "dispose", None)
            if callable(dispose):
                dispose()

async def _execute_overlay(
    overlay: OverlayEngine,
    command: str,
    *,
    sandbox: Any,
    timeout: int | None,
    stdin: str | None,
    description: str,
    agent_id: str,
    on_progress_line: Callable[[str], None] | None,
) -> OverlayRunOutcome:
    result = overlay.execute(
        command,
        sandbox=sandbox,
        timeout=timeout,
        stdin=stdin,
        description=description,
        agent_id=agent_id,
        on_progress_line=on_progress_line,
    )
    return await _maybe_await(result)


async def _maybe_await(value: Any) -> Any:
    if inspect.isawaitable(value):
        return await value
    return value


def _overlay_changes_to_changeset(upper_changes) -> list[Change]:
    changes: list[Change] = []
    for upper in upper_changes:
        if upper.kind == "whiteout":
            if upper.base_existed:
                changes.append(DeleteChange(path=upper.rel, source="shell_capture"))
            continue
        if upper.kind == "regular":
            changes.append(
                WriteChange(
                    path=upper.rel,
                    source="shell_capture",
                    final_content=upper.upper_bytes or b"",
                )
            )
            continue
        if upper.kind == "symlink":
            changes.append(
                SymlinkChange(
                    path=upper.rel,
                    target=(upper.upper_bytes or b"").decode("utf-8", errors="replace"),
                    source="shell_capture",
                )
            )
            continue
        if upper.kind == "opaque_dir":
            changes.append(
                OpaqueDirChange(
                    path=upper.rel,
                    kept_children=frozenset(_kept_children_for(upper.rel, upper_changes)),
                    source="shell_capture",
                )
            )
    return changes


def _kept_children_for(rel: str, upper_changes) -> set[str]:
    prefix = f"{rel}/"
    kept: set[str] = set()
    for item in upper_changes:
        if not item.rel.startswith(prefix):
            continue
        rest = item.rel[len(prefix):]
        if rest:
            kept.add(rest.split("/", 1)[0])
    return kept


async def _apply_changeset(
    changes: list[Change],
    *,
    changeset_applier: _ChangesetApplier | None,
) -> ChangesetResult:
    if not changes:
        return ChangesetResult(files=())
    if changeset_applier is None:
        return _pipeline_failure(
            changes[0].path,
            "OCC changeset applier is not configured",
        )

    result = await changeset_applier.apply_changeset(changes)
    if isinstance(result, ChangesetResult):
        return result
    return _pipeline_failure(
        changes[0].path,
        "OCC service prepared but did not commit the changeset",
    )


def _pipeline_failure(path: str, message: str) -> ChangesetResult:
    return ChangesetResult(
        files=(
            FileResult(
                path=path,
                status=FileStatus.FAILED,
                message=message,
            ),
        )
    )


def _shell_result_from_changeset(
    outcome: OverlayRunOutcome,
    result: ChangesetResult,
    *,
    workspace_root: str,
) -> ShellResult:
    """Project a :class:`ChangesetResult` into a :class:`ShellResult`."""
    committed = sorted({
        _absolutize(f.path, workspace_root)
        for f in result.files
        if is_published_status(f.status) and f.path
    })
    changed_paths = tuple(committed)

    if result.success:
        return ShellResult(
            result=outcome.stdout,
            exit_code=outcome.exit_code,
            changed_paths=changed_paths,
            warnings=tuple(outcome.warnings),
            overlay_run_timings=dict(outcome.overlay_run_timings),
            overlay_stage_timings=dict(outcome.overlay_stage_timings),
        )

    bad = next((f for f in result.files if not is_success_status(f.status)), None)
    if bad is None:
        return ShellResult(  # pragma: no cover - success/failure mismatch guard
            result=outcome.stdout,
            exit_code=outcome.exit_code,
            changed_paths=changed_paths,
            warnings=tuple(outcome.warnings),
            overlay_run_timings=dict(outcome.overlay_run_timings),
            overlay_stage_timings=dict(outcome.overlay_stage_timings),
        )
    reason = "patch_failed" if bad.status is FileStatus.ABORTED_OVERLAP else bad.status.value
    return ShellResult(
        result=outcome.stdout,
        exit_code=outcome.exit_code,
        changed_paths=changed_paths,
        warnings=tuple(outcome.warnings),
        overlay_run_timings=dict(outcome.overlay_run_timings),
        overlay_stage_timings=dict(outcome.overlay_stage_timings),
        conflict=ConflictInfo(
            reason=reason,
            conflict_file=_absolutize(bad.path, workspace_root) if bad.path else None,
            message=bad.message or reason,
        ),
    )


def _absolutize(rel: str, workspace_root: str) -> str:
    if not rel:
        return rel
    if rel.startswith("/"):
        return rel
    root = workspace_root.rstrip("/")
    return f"{root}/{rel}" if root else rel


__all__ = ["shell_pipeline"]
