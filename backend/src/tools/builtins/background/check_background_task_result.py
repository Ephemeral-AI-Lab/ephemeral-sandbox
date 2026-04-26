"""Built-in tool for fetching the result of a single background task."""

from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel

from tools.core.base import BaseTool, TextToolOutput, ToolExecutionContextService, ToolResult

from ._common import TASK_ID_FIELD, normalize_status, render_tool_command


class CheckBackgroundTaskResultInput(BaseModel):
    """Input for check_background_task_result tool."""
    task_id: str = TASK_ID_FIELD


def _peek_messages(tracked, n: int = 5) -> str:
    """Return the last *n* peek lines via the tracked task's progress provider."""
    provider = getattr(tracked, "progress_provider", None)
    if provider is None:
        return "(no progress snapshot available)"
    try:
        return provider(n)
    except Exception as exc:
        return f"[progress provider error: {exc}]"


def _subagent_terminal_called(tracked) -> bool:
    """Whether the subagent finished by calling its terminal tool.

    The flag is stamped by ``run_subagent`` on its returned ToolResult's
    metadata; the bg manager preserves the ToolResult on tracked.result.
    """
    if tracked.result is None:
        return False
    meta = tracked.result.metadata or {}
    return bool(meta.get("subagent_terminal_called"))


def _build_subagent_result(tracked, raw_status: str) -> tuple[str, str]:
    """Return (normalized_status, result) for a run_subagent task."""
    if raw_status == "running":
        return "running", _peek_messages(tracked, 5)

    if raw_status in ("completed", "delivered") and _subagent_terminal_called(tracked):
        return "finished", tracked.result.output if tracked.result else ""

    # Either crashed/cancelled, or completed without calling the terminal
    # tool — surface as failed and include the last 5 messages so the parent
    # can see what the subagent was doing.
    return "failed", _peek_messages(tracked, 5)


def _build_generic_result(tracked, raw_status: str) -> str:
    """Return result text for non-subagent tools (e.g. shell).

    No truncation — shell output is returned verbatim.
    """
    if raw_status == "running":
        if tracked.progress_lines:
            return "\n".join(tracked.progress_lines)
        return "[no output captured yet]"
    if tracked.result is None:
        return ""
    return tracked.result.output or ""


class CheckBackgroundTaskResultTool(BaseTool):
    """Fetch the current result of a single background task.

    Returns a JSON object: ``{id, status, tool_command, result}``.
    Works on running tasks (returns a snapshot) and on terminal tasks.
    """

    name: str = "check_background_task_result"
    description: str = (
        "Fetches the result of a background task by id. Returns "
        "{id, status (running|finished|failed), tool_command, result}. "
        "For run_subagent: result is the submit_exploration_result findings "
        "if finished, or the last 5 messages otherwise. For other tools "
        "(e.g. shell): result is the full output."
    )
    short_description: str = "Check a background task's result."
    input_model: type[BaseModel] = CheckBackgroundTaskResultInput
    output_model: type[BaseModel] = TextToolOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContextService) -> ToolResult:
        manager = context.get("background_task_manager")
        if manager is None:
            return ToolResult(
                output="ERROR: background task manager is not available.",
                is_error=True,
            )

        assert isinstance(arguments, CheckBackgroundTaskResultInput)
        tracked = manager.get_task(arguments.task_id) if hasattr(manager, "get_task") else None
        if tracked is None:
            return ToolResult(
                output=f"No background task found with ID: {arguments.task_id}",
                is_error=True,
            )

        raw_status = str(tracked.status)
        tool_command = render_tool_command(tracked.tool_name, tracked.tool_input)

        if tracked.tool_name == "run_subagent" or getattr(tracked, "task_type", "") == "subagent":
            status, result = _build_subagent_result(tracked, raw_status)
        else:
            status = normalize_status(raw_status)
            result = _build_generic_result(tracked, raw_status)

        # If the engine hasn't yet delivered this terminal task, mark it
        # delivered now so we don't get a duplicate [BACKGROUND COMPLETED]
        # message — the caller already has the result in this response.
        if status != "running" and raw_status in ("completed", "failed", "cancelled"):
            manager.collect_completed()

        payload: dict[str, Any] = {
            "id": tracked.task_id,
            "status": status,
            "tool_command": tool_command,
            "result": result,
        }
        return ToolResult(output=json.dumps(payload, indent=2), is_error=False)
