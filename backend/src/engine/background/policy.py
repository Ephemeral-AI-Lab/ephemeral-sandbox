"""Hard-coded engine background-manager wiring."""

from __future__ import annotations

from typing import Any

from engine.background.task_supervisor import SUBAGENT_TASK_TYPE

BACKGROUND_TOOL_INPUT_KEY = "_background_task"
SUBAGENT_LAUNCH_TOOL_NAMES = frozenset({"run_subagent"})
WORKFLOW_TOOL_NAMES = frozenset(
    {
        "delegate_workflow",
        "check_workflow_status",
        "cancel_workflow",
    }
)
PTY_SESSION_TOOL_NAMES = frozenset(
    {
        "cancel_pty_command",
        "check_pty_command_progress",
        "exec_command",
        "write_pty_command_stdin",
    }
)
GENERIC_BACKGROUND_TOOL_NAMES = frozenset({"shell"})


def is_explicit_generic_background_tool(
    tool: Any,
    tool_input: dict[str, Any] | None,
) -> bool:
    """Return whether a hidden internal key requests generic background dispatch."""
    return (
        getattr(tool, "name", "") in GENERIC_BACKGROUND_TOOL_NAMES
        and bool((tool_input or {}).get(BACKGROUND_TOOL_INPUT_KEY))
    )


def supports_explicit_generic_background(tool: Any) -> bool:
    """Return whether a tool can be internally launched as a generic background op."""
    return getattr(tool, "name", "") in GENERIC_BACKGROUND_TOOL_NAMES


def is_engine_background_tool(tool: Any) -> bool:
    """Return whether a tool must launch through BackgroundTaskSupervisor."""
    return (
        getattr(tool, "name", "") in SUBAGENT_LAUNCH_TOOL_NAMES
        or getattr(tool, "task_type", "") == SUBAGENT_TASK_TYPE
    )


def needs_background_manager(tool: Any) -> bool:
    """Return whether this tool surface needs the per-query background manager."""
    return (
        is_engine_background_tool(tool)
        or supports_explicit_generic_background(tool)
        or getattr(tool, "name", "") in PTY_SESSION_TOOL_NAMES
        or getattr(tool, "name", "") in WORKFLOW_TOOL_NAMES
    )
