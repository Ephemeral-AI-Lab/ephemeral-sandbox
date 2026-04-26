"""Shared implementation for the mode-entry tools."""

from __future__ import annotations

from tools.core.base import ToolExecutionContextService, ToolResult


def enter_secondary_mode(
    context: ToolExecutionContextService,
    *,
    target_mode: str,
    required_role: str,
    briefing: str,
    tool_name: str,
) -> ToolResult:
    """Apply the four mode-entry guards, then flip ``Task.mode`` to *target_mode*.

    All guards return ``is_error=True`` ToolResults so the dispatcher's normal
    error-result flow surfaces them to the model without flipping the mode.

    On success — including the idempotent "already in target mode" case — the
    returned ToolResult carries the mode briefing as ``output`` and
    ``mode_transition=target_mode``. The dispatcher reads the latter to update
    ``QueryContext.active_mode`` after the turn.
    """
    if context.get("agent_type") == "subagent":
        return ToolResult(
            output=(
                f"{tool_name}: rejected — subagent contexts cannot toggle "
                "the parent task's mode. Subagents run their own task with "
                "their own mode field."
            ),
            is_error=True,
        )

    role = context.get("role")
    if role != required_role:
        return ToolResult(
            output=(
                f"{tool_name}: rejected — this tool is {required_role}-only "
                f"(current role={role!r})."
            ),
            is_error=True,
        )

    tc = context.get("task_center")
    task_id = context.get("task_id")
    if tc is None or task_id is None:
        return ToolResult(
            output=f"{tool_name}: missing task_center or task_id in metadata",
            is_error=True,
        )

    task = tc.graph.get(task_id)
    if task.mode == target_mode:
        # Idempotent: re-deliver the briefing without flipping anything.
        return ToolResult(output=briefing, mode_transition=target_mode)
    if task.mode != "direct":
        terminals = _terminals_for_mode(context, task.mode)
        terminals_text = ", ".join(terminals) if terminals else "(none registered)"
        return ToolResult(
            output=(
                f"{tool_name}: rejected — task is already in mode "
                f"{task.mode!r}; cross-secondary transitions are not allowed. "
                f"Allowed terminals for {task.mode!r}: {terminals_text}. "
                "Exit the current mode via one of those terminals first."
            ),
            is_error=True,
        )

    task.mode = target_mode
    return ToolResult(output=briefing, mode_transition=target_mode)


def _terminals_for_mode(context: ToolExecutionContextService, mode_name: str) -> list[str]:
    """Best-effort lookup of *mode_name*'s terminals via the agent definition.

    The deny payload for cross-secondary attempts must name the current mode's
    terminals so the agent knows the exact escape hatch (spec §Failure Modes).
    Falls back to an empty list when the metadata is incomplete — the caller
    formats a generic "(none registered)" string in that case.
    """
    agent_def = context.get("agent_def")
    if agent_def is None:
        return []
    try:
        return list(agent_def.modes_by_name[mode_name].terminals)
    except (AttributeError, KeyError):
        return []
