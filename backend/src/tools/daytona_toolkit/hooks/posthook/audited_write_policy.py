"""Post-hook audited write policy: blocks / advises on committed paths."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.hooks import PostHookOutcome, ToolHookRegistry, default_registry
from tools.daytona_toolkit._audit import audited_write_outcome


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
    result: ToolResult,
) -> PostHookOutcome:
    del args
    raw_paths = result.metadata.get("changed_paths")
    if not isinstance(raw_paths, list):
        return PostHookOutcome()
    changed_paths = [str(path) for path in raw_paths if str(path or "").strip()]
    if not changed_paths:
        return PostHookOutcome()

    warnings, error = audited_write_outcome(
        context, changed_paths, tool_name=tool_name,
    )
    if error:
        return PostHookOutcome(has_error=True, error_message=error)
    return PostHookOutcome(advisories=tuple(warnings))


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_shell",
        "post",
        10,
        hook,
        name="daytona_shell:audited_write_policy",
    )
