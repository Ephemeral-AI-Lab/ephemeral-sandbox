"""Reject test files and test directories in submitted task scope paths."""

from __future__ import annotations

from pydantic import BaseModel

from team.core.scope import is_test_scope_path
from tools.core.base import ToolExecutionContext
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry

_SUBMISSION_TOOLS = ("submit_plan", "submit_replan")


def _forbidden_scope_paths(args: BaseModel) -> list[tuple[str, str]]:
    offenders: list[tuple[str, str]] = []
    new_tasks = getattr(args, "new_tasks", None)
    if not isinstance(new_tasks, list):
        return offenders
    for task in new_tasks:
        task_id = str(getattr(task, "id", "") or "<unknown>")
        scope_paths = getattr(task, "scope_paths", None)
        if not isinstance(scope_paths, list):
            continue
        for raw_path in scope_paths:
            if not isinstance(raw_path, str):
                continue
            path = raw_path.strip()
            if path and is_test_scope_path(path):
                offenders.append((task_id, path))
    return offenders


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
) -> PreHookOutcome:
    del tool_name, context
    offenders = _forbidden_scope_paths(args)
    if not offenders:
        return PreHookOutcome()

    rendered = "\n  - ".join(f"{task_id}: {path}" for task_id, path in offenders)
    return PreHookOutcome(
        has_error=True,
        error_message=(
            "test files and test directories cannot be used as scope_paths. "
            "Put verification commands and test targets in the task spec instead. "
            f"Offending scope_paths:\n  - {rendered}"
        ),
    )


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    for tool_name in _SUBMISSION_TOOLS:
        reg.register(
            tool_name,
            "pre",
            10,
            hook,
            name=f"{tool_name}:reject_test_scope_paths",
        )
