"""In-sandbox runtime pipelines."""

from __future__ import annotations

import inspect
from collections.abc import Callable, Sequence
from contextlib import contextmanager
from typing import Any

from sandbox.occ.changeset import ChangesetResult
from sandbox.occ.engine import LocalOCCEngine
from sandbox.overlay.engine import LocalOverlayEngine, OverlayEngine
from sandbox.overlay.types import ConflictInfo, OverlayRunOutcome, ShellResult
from sandbox.occ.types import EditSpec, OperationResult, WriteSpec


class _ApplyChangesetEngine:
    def __init__(self, apply_changeset: Callable[..., Any]) -> None:
        self._apply_changeset = apply_changeset

    def apply_changeset(self, *args: Any, **kwargs: Any) -> Any:
        return self._apply_changeset(*args, **kwargs)

    def dispose(self) -> None:
        return None


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
    """Run shell through overlay capture, then project OCC's changeset verdict."""
    owns_overlay = overlay_engine is None
    owns_occ = occ_engine is None and occ_apply_changeset is None
    overlay = overlay_engine or LocalOverlayEngine(
        sandbox_id=sandbox_id,
        workspace_root=workspace_root,
        daemon_local=True,
    )
    if occ_engine is not None:
        occ = occ_engine
    elif occ_apply_changeset is not None:
        occ = _ApplyChangesetEngine(occ_apply_changeset)
    else:
        occ = LocalOCCEngine(workspace_root=workspace_root)
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
        if outcome.overlay_rejected:
            return _overlay_reject_result(outcome)

        changeset_result = await _maybe_await(
            occ.apply_changeset(
                outcome.upper_changes,
                agent_id=agent_id,
                edit_type="svc_cmd_overlay",
                description=description or "shell overlay",
            )
        )
        return _changeset_result(outcome, changeset_result)
    finally:
        if owns_overlay:
            dispose = getattr(overlay, "dispose", None)
            if callable(dispose):
                dispose()
        if owns_occ:
            dispose = getattr(occ, "dispose", None)
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
    try:
        result = overlay.execute(
            command,
            sandbox=sandbox,
            timeout=timeout,
            stdin=stdin,
            description=description,
            agent_id=agent_id,
            on_progress_line=on_progress_line,
        )
    except TypeError:
        result = overlay.execute(  # type: ignore[misc,call-arg]
            sandbox,
            command,
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


def _overlay_reject_result(outcome: OverlayRunOutcome) -> ShellResult:
    conflict = outcome.conflict
    reject = outcome.policy_reject
    if conflict is None:
        reason = reject.reason if reject is not None else "overlay_rejected"
        conflict = ConflictInfo(
            reason=reason,
            conflict_file=reject.paths[0] if reject is not None and reject.paths else None,
            message=reason,
        )
    return ShellResult(
        result=outcome.stdout,
        exit_code=outcome.exit_code,
        warnings=tuple(outcome.warnings),
        overlay_run_timings=dict(outcome.overlay_run_timings),
        overlay_stage_timings=dict(outcome.overlay_stage_timings),
        conflict=conflict,
    )


def _changeset_result(
    outcome: OverlayRunOutcome,
    changeset_result: ChangesetResult,
) -> ShellResult:
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


__all__ = ["edit_pipeline", "shell_pipeline", "write_pipeline"]
