"""Add successful write targets to write_scope."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.hooks import PostHookOutcome, ToolHookRegistry, default_registry
from tools.daytona_toolkit._daytona_utils import _extend_write_scope, _resolve_path


def _scope_added_advisory(context: ToolExecutionContext, added_path: str) -> str:
    current = context.metadata.get("write_scope")
    if isinstance(current, list):
        scope_paths = [str(path) for path in current]
    else:
        scope_paths = []
    rendered_scope = ", ".join(scope_paths) if scope_paths else "<none>"
    return (
        f"Scope path added: {added_path}. "
        f"Current scope_paths: {rendered_scope}."
    )


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
    result: ToolResult,
) -> PostHookOutcome:
    del tool_name
    if result.is_error:
        return PostHookOutcome()
    changed = result.metadata.get("changed_paths")
    if not isinstance(changed, list) or not changed:
        return PostHookOutcome()

    file_path = getattr(args, "file_path", None)
    if not isinstance(file_path, str):
        return PostHookOutcome()

    added_path = _extend_write_scope(context, _resolve_path(file_path, context))
    if added_path is None:
        return PostHookOutcome()
    return PostHookOutcome(advisories=(_scope_added_advisory(context, added_path),))


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_write_file",
        "post",
        10,
        hook,
        name="daytona_write_file:extend_write_scope_on_success",
    )
