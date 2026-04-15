"""Built-in tool for cancelling background tasks."""

from __future__ import annotations

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, ToolExecutionContext, ToolResult

from ._common import TASK_ID_FIELD


class CancelBackgroundTaskInput(BaseModel):
    """Input for cancel_background_task tool."""
    task_id: str = TASK_ID_FIELD
    reason: str = Field(
        default="",
        description="Optional reason for cancellation.",
    )


class CancelBackgroundTaskTool(BaseTool):
    """Cancel a running background task.

    Stops the specified background task. The task will be marked as
    cancelled and its partial output (if any) will be available via
    check_background_progress.
    """

    name: str = "cancel_background_task"
    description: str = (
        "Cancel a running background task by its task ID. "
        "Use check_background_progress first to find the task ID. "
        "task_id is REQUIRED — pass the exact id string (e.g. \"bg_1\"). "
        "Pass \"auto\" to cancel the sole running task when exactly one is running."
    )
    short_description: str = "Cancel a background task."
    input_model: type[BaseModel] = CancelBackgroundTaskInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        manager = context.metadata.get("background_task_manager")
        if manager is None:
            return ToolResult(
                output="No background task manager available.",
                is_error=True,
            )

        assert isinstance(arguments, CancelBackgroundTaskInput)

        task_id = arguments.task_id

        # `all` is a sentinel supported by check/wait — but cancel is a
        # mutating action and we want the LLM to make an explicit choice
        # per task. Reject it loudly instead of failing later inside
        # manager.cancel("all", ...).
        if task_id == "all":
            return ToolResult(
                output=(
                    "ERROR: cancel_background_task does not support task_id=\"all\". "
                    "Cancel each task explicitly by its task_id, or call "
                    "check_background_progress to list them first."
                ),
                is_error=True,
            )

        # Disambiguation guard: `task_id="auto"` resolves to the sole
        # running task, or returns a listing if there are 0 / >1.
        # Schema makes task_id required so we should never see empty
        # strings here, but we handle them defensively the same way.
        if task_id == "auto" or not task_id:
            snapshot = manager.get_status()
            running = [s for s in snapshot if s.get("status") == "running"]
            if len(running) == 0:
                return ToolResult(
                    output="No background tasks are running — nothing to cancel.",
                    is_error=False,
                )
            if len(running) == 1:
                task_id = running[0]["task_id"]
            else:
                listing = "\n".join(
                    f"  - task_id=\"{s['task_id']}\"  ({s.get('task_note') or s.get('tool_name')})"
                    for s in running
                )
                return ToolResult(
                    output=(
                        "ERROR: multiple background tasks are running and `task_id` "
                        "was not provided. You MUST copy one of the exact task_id "
                        "strings below into the `task_id` argument.\n"
                        f"Running tasks:\n{listing}\n"
                        "Example: cancel_background_task(task_id=\"<one of the above>\", reason=\"...\")"
                    ),
                    is_error=True,
                )

        tracked = manager.get_task(task_id) if hasattr(manager, "get_task") else None
        cancelled = await manager.cancel(task_id, arguments.reason)

        if cancelled:
            reason_msg = f" Reason: {arguments.reason}" if arguments.reason else ""
            if tracked is not None and getattr(tracked, "task_type", "") == "subagent":
                return ToolResult(
                    output=(
                        f"Background task {task_id} early-stop requested.{reason_msg} "
                        "The subagent was interrupted and will salvage any partial "
                        "result before it reaches a terminal state."
                    ),
                    is_error=False,
                )
            return ToolResult(
                output=f"Background task {task_id} cancelled.{reason_msg}",
                is_error=False,
            )

        return ToolResult(
            output=f"Could not cancel task {task_id}. "
            "It may have already completed or does not exist.",
            is_error=True,
        )
