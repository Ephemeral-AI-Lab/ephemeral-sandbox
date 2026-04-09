"""Built-in tool for querying background task status."""

from __future__ import annotations

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, ToolExecutionContext, ToolResult

from ._common import (
    TASK_ID_FIELD,
    apply_last_n_lines,
    build_background_snapshot_metadata,
    render_background_snapshot,
)


class CheckBackgroundProgressInput(BaseModel):
    """Input for check_background_progress tool."""
    task_id: str = TASK_ID_FIELD
    last_n_lines: int = Field(
        default=20,
        ge=1,
        description=(
            "Number of recent output lines to include. For subagent-style "
            "tasks (run_subagent), this is interpreted as recent messages "
            "and is hard-capped at 10."
        ),
    )


class CheckBackgroundProgressTool(BaseTool):
    """Query the status of background tasks (non-blocking)."""

    name: str = "check_background_progress"
    description: str = (
        "Check background task status without blocking. Use this to inspect live output and decide "
        "whether to keep waiting, act on the result, or cancel. Pass an exact task_id like "
        "\"bg_1\" or \"all\"."
    )
    input_model: type[BaseModel] = CheckBackgroundProgressInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        manager = context.metadata.get("background_task_manager")
        if manager is None:
            return ToolResult(
                output=(
                    "ERROR: background task manager is not available in this "
                    "context — no background tasks can be queried."
                ),
                is_error=True,
            )

        target_id = None if arguments.task_id == "all" else arguments.task_id
        status = manager.get_status(task_id=target_id, last_n=arguments.last_n_lines)
        if target_id is None:
            running = [entry for entry in status if entry.get("status") == "running"]
            if running:
                status = running
                manager.mark_progress_checked()

        if not status:
            if target_id is not None:
                return ToolResult(
                    output=f"No background task found with ID: {target_id}",
                    is_error=True,
                )
            return ToolResult(output="No background tasks.", is_error=False)

        if target_id is not None:
            manager.mark_progress_checked(target_id)

        apply_last_n_lines(status, arguments.last_n_lines)
        return ToolResult(
            output=render_background_snapshot("progress", status),
            is_error=False,
            metadata=build_background_snapshot_metadata(
                "progress",
                arguments.task_id,
                status,
            ),
        )

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True
