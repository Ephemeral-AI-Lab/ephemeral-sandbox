"""Host-side enter/exit coroutines for isolated workspace tools.

The Rust daemon is the isolated-workspace authority. This module keeps only the
host concerns: local background-task gating, per-agent background drain, typed
result decoding, and lifecycle audit timing.
"""

from __future__ import annotations

import os

from sandbox._shared.models import (
    EnterIsolatedWorkspaceRequest,
    EnterIsolatedWorkspaceResult,
    ExitIsolatedWorkspaceRequest,
    ExitIsolatedWorkspaceResult,
    LifecycleError,
)
from sandbox.audit.lifecycle import lifecycle_operation
from sandbox.host.daemon_client import _DaemonDispatchError, call_daemon_api


async def enter_isolated_workspace(
    request: EnterIsolatedWorkspaceRequest,
    *,
    background_manager: object | None = None,
    sandbox_id: str = "",
) -> EnterIsolatedWorkspaceResult:
    agent_id = request.caller.agent_id
    try:
        local_count = _count_by_agent(background_manager, agent_id)
        daemon_count = await _daemon_command_session_count(sandbox_id, agent_id)
        in_flight = max(local_count, daemon_count)
        if in_flight > 0:
            return EnterIsolatedWorkspaceResult(
                success=False,
                error=LifecycleError(
                    kind="ephemeral_jobs_in_flight",
                    message="sandbox-bound background tasks are still running",
                    details={"count": str(in_flight)},
                ),
            )
        async with lifecycle_operation(
            kind="enter_isolated_workspace",
            agent_id=agent_id,
            audit_path=os.environ.get("EOS_WORKSPACE_LIFECYCLE_AUDIT_PATH"),
        ) as timings:
            if not sandbox_id:
                return _missing_sandbox_id_enter(timings=dict(timings))
            return await _daemon_enter(sandbox_id, request, timings=dict(timings))
    except RuntimeError as exc:
        return EnterIsolatedWorkspaceResult(
            success=False,
            error=LifecycleError(
                kind="command_session_count_unavailable",
                message=str(exc),
                details={"sandbox_id": sandbox_id},
            ),
        )


async def exit_isolated_workspace(
    request: ExitIsolatedWorkspaceRequest,
    *,
    background_manager: object | None = None,
    sandbox_id: str = "",
) -> ExitIsolatedWorkspaceResult:
    agent_id = request.caller.agent_id
    async with lifecycle_operation(
        kind="exit_isolated_workspace",
        agent_id=agent_id,
        audit_path=os.environ.get("EOS_WORKSPACE_LIFECYCLE_AUDIT_PATH"),
    ) as timings:
        evicted_background_tasks = await _cancel_by_agent(
            background_manager,
            agent_id,
            grace_s=request.grace_s,
        )
        if sandbox_id:
            return await _daemon_exit(
                sandbox_id,
                request,
                evicted_background_tasks=evicted_background_tasks,
                timings=dict(timings),
            )
        return _missing_sandbox_id_exit(
            evicted_background_tasks=evicted_background_tasks,
            timings=dict(timings),
        )


async def _daemon_enter(
    sandbox_id: str,
    request: EnterIsolatedWorkspaceRequest,
    *,
    timings: dict[str, float],
) -> EnterIsolatedWorkspaceResult:
    try:
        response = await call_daemon_api(
            sandbox_id,
            "api.isolated_workspace.enter",
            {
                "agent_id": request.caller.agent_id,
                "layer_stack_root": request.layer_stack_root,
            },
            layer_stack_root=request.layer_stack_root,
            timeout=180,
        )
    except _DaemonDispatchError as exc:
        return EnterIsolatedWorkspaceResult(
            success=False,
            timings=timings,
            error=_lifecycle_error_from_dispatch(exc),
        )
    error = response.get("error")
    if error is not None:
        return EnterIsolatedWorkspaceResult(
            success=False,
            timings=timings,
            error=_lifecycle_error_from_mapping(error),
        )
    return EnterIsolatedWorkspaceResult(
        success=bool(response.get("success", True)),
        manifest_version=str(response.get("manifest_version") or ""),
        manifest_root_hash=str(response.get("manifest_root_hash") or ""),
        timings=timings,
    )


async def _daemon_exit(
    sandbox_id: str,
    request: ExitIsolatedWorkspaceRequest,
    *,
    evicted_background_tasks: int,
    timings: dict[str, float],
) -> ExitIsolatedWorkspaceResult:
    try:
        response = await call_daemon_api(
            sandbox_id,
            "api.isolated_workspace.exit",
            {"agent_id": request.caller.agent_id},
            timeout=180,
        )
    except _DaemonDispatchError as exc:
        return ExitIsolatedWorkspaceResult(
            success=False,
            timings=timings,
            error=_lifecycle_error_from_dispatch(exc),
        )
    error = response.get("error")
    if error is not None:
        return ExitIsolatedWorkspaceResult(
            success=False,
            timings=timings,
            error=_lifecycle_error_from_mapping(error),
        )
    phases = dict(response.get("phases_ms") or {})
    phases["evicted_background_tasks"] = float(evicted_background_tasks)
    timings.update({str(key): float(value) for key, value in phases.items()})
    return ExitIsolatedWorkspaceResult(
        success=bool(response.get("success", True)),
        evicted_upperdir_bytes=int(response.get("evicted_upperdir_bytes") or 0),
        lifetime_s=float(response.get("lifetime_s") or 0.0),
        phases_ms=phases,
        timings=timings,
    )


def _lifecycle_error_from_dispatch(exc: _DaemonDispatchError) -> LifecycleError:
    return LifecycleError(
        kind=str(exc.kind or "internal_error"),
        message=str(exc.message or ""),
        details={str(k): str(v) for k, v in (exc.details or {}).items()},
    )


def _lifecycle_error_from_mapping(error: object) -> LifecycleError:
    if not isinstance(error, dict):
        return LifecycleError(kind="internal_error", message=str(error))
    details = error.get("details")
    return LifecycleError(
        kind=str(error.get("kind") or "internal_error"),
        message=str(error.get("message") or ""),
        details={str(k): str(v) for k, v in (details if isinstance(details, dict) else {}).items()},
    )


def _missing_sandbox_id_enter(*, timings: dict[str, float]) -> EnterIsolatedWorkspaceResult:
    return EnterIsolatedWorkspaceResult(
        success=False,
        timings=timings,
        error=LifecycleError(
            kind="sandbox_id_required",
            message="isolated workspace enter requires a Rust sandbox daemon sandbox_id",
        ),
    )


def _missing_sandbox_id_exit(
    *,
    evicted_background_tasks: int,
    timings: dict[str, float],
) -> ExitIsolatedWorkspaceResult:
    phases = {"evicted_background_tasks": float(evicted_background_tasks)}
    timings.update(phases)
    return ExitIsolatedWorkspaceResult(
        success=False,
        phases_ms=phases,
        timings=timings,
        error=LifecycleError(
            kind="sandbox_id_required",
            message="isolated workspace exit requires a Rust sandbox daemon sandbox_id",
        ),
    )


def _count_by_agent(background_manager: object | None, agent_id: str) -> int:
    if background_manager is None:
        return 0
    counter = getattr(background_manager, "count_by_agent", None)
    if not callable(counter):
        return 0
    return int(counter(agent_id))


async def _cancel_by_agent(
    background_manager: object | None,
    agent_id: str,
    *,
    grace_s: float,
) -> int:
    if background_manager is None:
        return 0
    canceller = getattr(background_manager, "cancel_by_agent", None)
    if not callable(canceller):
        return 0
    return int(await canceller(agent_id, grace_s=grace_s))


async def _daemon_command_session_count(sandbox_id: str, agent_id: str) -> int:
    if not sandbox_id:
        return 0
    try:
        import sandbox.api as sandbox_api

        return await sandbox_api.command_session_count(sandbox_id, agent_id)
    except Exception as exc:
        raise RuntimeError("daemon command session count check failed") from exc


__all__ = ["enter_isolated_workspace", "exit_isolated_workspace"]
