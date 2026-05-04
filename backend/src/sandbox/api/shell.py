"""Public sandbox shell verb."""

from __future__ import annotations

from sandbox.api.utils.models import ConflictInfo, ShellRequest, ShellResult
from sandbox.api.raw_exec import raw_exec
from sandbox.api.utils.shell_routing import is_read_only_pipeline
from sandbox.overlay.client import OverlayClient


async def shell(sandbox_id: str, request: ShellRequest) -> ShellResult:
    """Run one guarded shell command through the overlay runtime peer."""
    if is_read_only_pipeline(request.command) and request.stdin is None:
        return await _raw_shell(sandbox_id, request)

    result = await OverlayClient(
        sandbox_id,
        workspace_root=request.cwd or "/workspace",
        timeout=request.timeout or 300,
    ).shell(
        request.command,
        timeout=request.timeout,
        stdin=request.stdin,
        description=request.description or "shell",
        agent_id=request.actor.agent_id,
    )
    conflict = _conflict_from_overlay(result.conflict)
    return ShellResult(
        success=conflict is None,
        exit_code=result.exit_code,
        stdout=result.result,
        stderr="",
        changed_paths=tuple(result.changed_paths),
        status="ok" if conflict is None else "error",
        conflict=conflict,
        conflict_reason=conflict.message if conflict is not None else None,
        warnings=tuple(result.warnings),
    )


async def _raw_shell(sandbox_id: str, request: ShellRequest) -> ShellResult:
    result = await raw_exec(
        sandbox_id,
        request.command,
        cwd=request.cwd,
        timeout=request.timeout,
    )
    return ShellResult(
        success=True,
        exit_code=result.exit_code,
        stdout=result.stdout,
        stderr=result.stderr,
        changed_paths=(),
        status="ok",
        conflict=None,
        conflict_reason=None,
        warnings=(),
    )


def _conflict_from_overlay(conflict: object | None) -> ConflictInfo | None:
    if conflict is None:
        return None
    return ConflictInfo(
        reason=str(getattr(conflict, "reason", "")),
        conflict_file=getattr(conflict, "conflict_file", None),
        message=str(getattr(conflict, "message", "")),
    )


__all__ = ["shell"]
