"""Built-in tool for blocking until all background tasks complete."""

from __future__ import annotations

import asyncio
import time

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, TextToolOutput, ToolExecutionContextService, ToolResult

from ._common import (
    build_background_snapshot_metadata,
    normalize_status,
    render_background_snapshot,
    render_tool_command,
)


class WaitBackgroundTasksInput(BaseModel):
    """Input for wait_background_tasks tool."""
    timeout: float = Field(
        default=30,
        ge=1,
        le=300,
        description=(
            "Maximum seconds to block waiting for ALL background tasks to "
            "settle. Must be in [1, 300]; values outside this range are "
            "rejected by schema validation."
        ),
    )


def _snapshot_all(manager) -> list[dict[str, str]]:
    """Build the [{task_id, status, tool_command}] list for every tracked task."""
    out: list[dict[str, str]] = []
    for tracked in manager.iter_all():
        out.append({
            "task_id": tracked.task_id,
            "status": normalize_status(tracked.status),
            "tool_command": render_tool_command(tracked.tool_name, tracked.tool_input),
        })
    return out


class WaitBackgroundTasksTool(BaseTool):
    """Block until all background tasks complete or timeout.

    Returns a compact snapshot per task: ``{task_id, status, tool_command}``.
    Use ``check_background_task_result(task_id)`` to fetch the actual result
    of a finished/failed task.
    """

    name: str = "wait_background_tasks"
    description: str = (
        "Blocks until every running background task settles, or the timeout "
        "expires. Returns one entry per task with its task_id, status "
        "(running|finished|failed), and tool_command."
    )
    short_description: str = "Wait for all background tasks."
    input_model: type[BaseModel] = WaitBackgroundTasksInput
    output_model: type[BaseModel] = TextToolOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContextService) -> ToolResult:
        manager = context.get("background_task_manager")
        if manager is None:
            return ToolResult(
                output=(
                    "ERROR: background task manager is not available in this "
                    "context — no background tasks can be waited on."
                ),
                is_error=True,
            )

        assert isinstance(arguments, WaitBackgroundTasksInput)

        if not list(manager.iter_all()):
            return ToolResult(
                output=render_background_snapshot("wait_no_tasks", []),
                is_error=False,
                metadata=build_background_snapshot_metadata(
                    "wait_no_tasks", "all", [],
                ),
            )

        timeout = arguments.timeout
        start = time.monotonic()
        running = [t.asyncio_task for t in manager.iter_running()]
        if running:
            try:
                await asyncio.wait(
                    running,
                    timeout=timeout,
                    return_when=asyncio.ALL_COMPLETED,
                )
            except Exception:
                pass

        # Mark any newly completed tasks as DELIVERED so the engine's normal
        # delivery path doesn't double-emit BACKGROUND COMPLETED messages —
        # this tool's response is already telling the caller everything.
        manager.collect_completed()

        elapsed = time.monotonic() - start
        statuses = _snapshot_all(manager)
        timed_out = manager.has_pending()

        if timed_out:
            return ToolResult(
                output=render_background_snapshot(
                    "wait_timed_out", statuses, elapsed_seconds=elapsed,
                ),
                is_error=False,
                metadata=build_background_snapshot_metadata(
                    "wait_timed_out", "all", statuses,
                    elapsed_seconds=elapsed,
                ),
            )
        return ToolResult(
            output=render_background_snapshot("wait_completed", statuses),
            is_error=False,
            metadata=build_background_snapshot_metadata(
                "wait_completed", "all", statuses,
                elapsed_seconds=elapsed,
            ),
        )
