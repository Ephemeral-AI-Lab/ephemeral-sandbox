"""Public sandbox shell verb."""

from __future__ import annotations

from sandbox.api.models import ConflictInfo, ShellRequest, ShellResult
from sandbox.overlay.client import OverlayClient
from sandbox.overlay.types import ConflictInfo as OverlayConflictInfo


async def shell(sandbox_id: str, request: ShellRequest) -> ShellResult:
    """Run one guarded shell command through the overlay runtime peer."""
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


def _conflict_from_overlay(conflict: OverlayConflictInfo | None) -> ConflictInfo | None:
    if conflict is None:
        return None
    return ConflictInfo(
        reason=conflict.reason,
        conflict_file=conflict.conflict_file,
        message=conflict.message,
    )


__all__ = ["shell"]
