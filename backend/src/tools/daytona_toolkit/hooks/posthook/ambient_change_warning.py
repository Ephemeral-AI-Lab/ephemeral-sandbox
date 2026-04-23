"""Warn when daytona_shell changes files outside its audited target paths."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.hooks import PostHookOutcome, ToolHookRegistry, default_registry

_MAX_RENDERED = 5


def _format(paths: list[str]) -> str:
    rendered = ", ".join(paths[:_MAX_RENDERED])
    if len(paths) > _MAX_RENDERED:
        rendered += f", ... ({len(paths)} total)"
    return (
        "Workspace changed during this shell command, but coordinated daytona_shell "
        "shell commands are runtime-only; treating changed paths as ambient "
        f"concurrent edits: {rendered}"
    )


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
    result: ToolResult,
) -> PostHookOutcome:
    del tool_name, args, context
    raw = result.metadata.get("ambient_changed_paths")
    if not isinstance(raw, list):
        return PostHookOutcome()
    paths = [str(p) for p in raw if str(p or "").strip()]
    if not paths:
        return PostHookOutcome()
    return PostHookOutcome(advisories=(_format(paths),))


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_shell",
        "post",
        20,
        hook,
        name="daytona_shell:ambient_change_warning",
    )
