"""In-sandbox runtime pipelines."""

from __future__ import annotations

import inspect
from collections.abc import Callable, Sequence
from contextlib import contextmanager
from typing import Any

from sandbox.occ.changeset.builders import overlay_changes_to_changeset
from sandbox.occ.changeset.legacy import LegacyChangesetResult
from sandbox.occ.changeset.types import ChangesetResult, FileStatus
from sandbox.occ.content.gitignore_oracle import GitignoreOracle
from sandbox.occ.content.manager import ContentManager
from sandbox.occ.direct.direct_merge_coordinator import DirectMergeCoordinator
from sandbox.occ.engine import LocalOCCEngine
from sandbox.occ.gated.gated_coordinator import OCCGatedCoordinator
from sandbox.occ.orchestrator import ChangesetOrchestrator
from sandbox.occ.types import EditSpec, OperationResult, WriteSpec
from sandbox.overlay.engine import OverlayCaptureEngine, OverlayEngine
from sandbox.overlay.types import OverlayRunOutcome
from sandbox.runtime.types import ConflictInfo, ShellResult


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
    occ_engine: Any | None = None,
    occ_apply_changeset: Callable[..., Any] | None = None,
    overlay_sandbox: Any = None,
    on_progress_line: Callable[[str], None] | None = None,
) -> ShellResult:
    """Run shell through overlay capture, then project the gate's verdict."""
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

        # Test injection paths still expect the legacy upper_changes
        # signature returning LegacyChangesetResult.
        if occ_engine is not None:
            legacy = await _maybe_await(
                occ_engine.apply_changeset(
                    outcome.upper_changes,
                    agent_id=agent_id,
                    edit_type="svc_cmd_overlay",
                    description=description or "shell overlay",
                )
            )
            return _legacy_shell_result(outcome, legacy)
        if occ_apply_changeset is not None:
            legacy = await _maybe_await(
                occ_apply_changeset(
                    outcome.upper_changes,
                    agent_id=agent_id,
                    edit_type="svc_cmd_overlay",
                    description=description or "shell overlay",
                )
            )
            return _legacy_shell_result(outcome, legacy)

        # Default path: build typed changes and run them through the new gate.
        typed_changes = overlay_changes_to_changeset(outcome.upper_changes)
        content = ContentManager(workspace_root)
        orchestrator = ChangesetOrchestrator(
            gitignore=GitignoreOracle(workspace_root),
            direct=DirectMergeCoordinator(content),
            gated=OCCGatedCoordinator(content),
        )
        result = await orchestrator.apply(typed_changes)
        return _shell_result_from_changeset(outcome, result, workspace_root=workspace_root)
    finally:
        if owns_overlay:
            dispose = getattr(overlay, "dispose", None)
            if callable(dispose):
                dispose()


@contextmanager
def _occ_engine(workspace_root: str):
    engine = LocalOCCEngine(workspace_root=workspace_root)
    try:
        yield engine
    finally:
        engine.dispose()


def edit_pipeline(
    specs: Sequence[EditSpec] | EditSpec,
    *,
    workspace_root: str = "/workspace",
    agent_id: str = "",
    description: str = "",
) -> OperationResult:
    """Apply a batch of edit specs and commit once through OCC."""
    with _occ_engine(workspace_root) as engine:
        return engine.edit_file(
            specs,
            agent_id=agent_id,
            description=description,
        )


def write_pipeline(
    specs: Sequence[WriteSpec] | WriteSpec,
    *,
    workspace_root: str = "/workspace",
    agent_id: str = "",
    description: str = "",
) -> OperationResult:
    """Write files and commit once through OCC."""
    with _occ_engine(workspace_root) as engine:
        return engine.write_file(
            specs,
            agent_id=agent_id,
            description=description,
        )


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


def _legacy_shell_result(
    outcome: OverlayRunOutcome,
    changeset_result: LegacyChangesetResult,
) -> ShellResult:
    """Project the legacy ``LegacyChangesetResult`` shape (test-injection path)."""
    changed_paths = tuple(
        sorted({*changeset_result.ledgered, *changeset_result.direct_merged})
    )
    if changeset_result.success:
        return ShellResult(
            result=outcome.stdout,
            exit_code=outcome.exit_code,
            changed_paths=changed_paths,
            warnings=tuple(outcome.warnings),
            overlay_run_timings=dict(outcome.overlay_run_timings),
            overlay_stage_timings=dict(outcome.overlay_stage_timings),
        )
    message = changeset_result.conflict_reason or changeset_result.status
    conflict = ConflictInfo(
        reason=changeset_result.conflict_reason or "occ_conflict",
        conflict_file=changeset_result.conflict_file,
        message=message,
    )
    return ShellResult(
        result=outcome.stdout,
        exit_code=outcome.exit_code,
        changed_paths=changed_paths,
        warnings=tuple(outcome.warnings),
        overlay_run_timings=dict(outcome.overlay_run_timings),
        overlay_stage_timings=dict(outcome.overlay_stage_timings),
        conflict=conflict,
    )


def _shell_result_from_changeset(
    outcome: OverlayRunOutcome,
    result: ChangesetResult,
    *,
    workspace_root: str,
) -> ShellResult:
    """Project the new ``ChangesetResult`` (per-file ``FileResult``) shape."""
    committed = sorted({
        _absolutize(f.path, workspace_root)
        for f in result.files
        if f.status is FileStatus.COMMITTED and f.path
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

    bad = next((f for f in result.files if f.status is not FileStatus.COMMITTED), None)
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


__all__ = ["edit_pipeline", "shell_pipeline", "write_pipeline"]
