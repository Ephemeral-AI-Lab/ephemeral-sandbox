"""Built-in tool for blocking until background tasks complete."""

from __future__ import annotations

import time

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, TextToolOutput, ToolExecutionContext, ToolResult

from ._common import (
    TASK_ID_FIELD,
    apply_last_n_lines,
    build_background_snapshot_metadata,
    render_background_snapshot,
)


class WaitForBackgroundTaskInput(BaseModel):
    """Input for wait_for_background_task tool."""
    task_id: str = TASK_ID_FIELD
    timeout: float = Field(
        default=30,
        ge=1,
        le=300,
        description=(
            "Maximum seconds to block waiting. Must be in [1, 300]; "
            "values outside this range are rejected by schema validation."
        ),
    )
    last_n_lines: int = Field(
        default=20,
        ge=1,
        description="Number of output lines to include for completed tasks.",
    )


class WaitForBackgroundTaskTool(BaseTool):
    """Block until background task(s) complete or timeout.

    Suspends execution server-side so the LLM does not need to poll in tight
    loops. Use this only when there is no foreground work to do.
    """

    name: str = "wait_for_background_task"
    description: str = (
        "Block server-side until background task(s) complete or the timeout expires. "
        "Use this only when you have no foreground work left or after recent progress "
        "shows the task is healthy enough to join."
    )
    short_description: str = "Wait for background tasks."
    input_model: type[BaseModel] = WaitForBackgroundTaskInput
    output_model: type[BaseModel] = TextToolOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        manager = context.metadata.get("background_task_manager")
        if manager is None:
            return ToolResult(
                output=(
                    "ERROR: background task manager is not available in this "
                    "context — no background tasks can be waited on."
                ),
                is_error=True,
            )

        # Schema enforces 1 <= timeout <= 300; no further clamping needed.
        timeout = arguments.timeout
        wait_for_all = arguments.task_id == "all"
        target_id: str | None = None if wait_for_all else arguments.task_id

        # ---- task_id="all" branch ----
        if wait_for_all:
            snapshot = manager.get_status()
            if not any(s.get("status") == "running" for s in snapshot):
                fresh = [
                    s for s in snapshot
                    if s.get("status") in ("completed", "failed", "cancelled")
                ]
                if fresh:
                    apply_last_n_lines(fresh, arguments.last_n_lines)
                    return ToolResult(
                        output=render_background_snapshot("wait_completed", fresh),
                        is_error=False,
                        metadata=build_background_snapshot_metadata(
                            "wait_completed",
                            arguments.task_id,
                            fresh,
                        ),
                    )
                delivered = [s for s in snapshot if s.get("status") == "delivered"]
                if delivered:
                    apply_last_n_lines(delivered, arguments.last_n_lines)
                    return ToolResult(
                        output=render_background_snapshot(
                            "wait_no_tasks", delivered
                        ),
                        is_error=False,
                        metadata=build_background_snapshot_metadata(
                            "wait_no_tasks",
                            arguments.task_id,
                            delivered,
                        ),
                    )
                return ToolResult(
                    output=render_background_snapshot("wait_no_tasks", []),
                    is_error=False,
                    metadata=build_background_snapshot_metadata(
                        "wait_no_tasks",
                        arguments.task_id,
                        [],
                    ),
                )

        # ---- specific task_id branch ----
        if target_id is not None:
            task_statuses = manager.get_status(target_id)
            if not task_statuses:
                return ToolResult(
                    output=f"No background task found with ID: {target_id}",
                    is_error=True,
                )
            if task_statuses[0].get("status") != "running":
                apply_last_n_lines(task_statuses, arguments.last_n_lines)
                notice = (
                    f"[ALREADY_COMPLETED] Task {target_id} had already finished "
                    "before this wait call was issued — no waiting occurred. "
                    "Your assumption that it was still running is stale; update "
                    "your mental model from the status payload below and do "
                    "not poll or wait on this task id again."
                )
                return ToolResult(
                    output=f"{notice}\n{render_background_snapshot('progress', task_statuses)}",
                    is_error=False,
                    metadata=build_background_snapshot_metadata(
                        "wait_completed",
                        arguments.task_id,
                        task_statuses,
                    ),
                )

        # Wait loop
        start = time.monotonic()
        while True:
            elapsed = time.monotonic() - start
            remaining = timeout - elapsed
            if remaining <= 0:
                break

            if wait_for_all:
                # wait_any() consumes completion events via collect_completed,
                # which is fine here because the caller asked to wait for
                # *every* task — the engine will still see all of them.
                await manager.wait_any(timeout=remaining)
                if not manager.has_pending():
                    break
                continue

            # Specific task: use wait_for() so completions of *other*
            # background tasks remain queued for the engine's normal
            # delivery path (otherwise they'd be silently consumed).
            await manager.wait_for(target_id, timeout=remaining)
            task_statuses = manager.get_status(target_id)
            if not task_statuses or task_statuses[0].get("status") != "running":
                break

        elapsed = time.monotonic() - start
        status = manager.get_status(target_id)
        apply_last_n_lines(status, arguments.last_n_lines)

        if wait_for_all:
            timed_out = manager.has_pending()
        else:
            timed_out = bool(status) and status[0].get("status") == "running"

        if timed_out:
            output = render_background_snapshot(
                "wait_timed_out",
                status,
                elapsed_seconds=elapsed,
            )
            metadata = build_background_snapshot_metadata(
                "wait_timed_out",
                arguments.task_id,
                status,
                elapsed_seconds=elapsed,
            )
        else:
            output = render_background_snapshot("wait_completed", status)
            metadata = build_background_snapshot_metadata(
                "wait_completed",
                arguments.task_id,
                status,
                elapsed_seconds=elapsed,
            )

        return ToolResult(output=output, is_error=False, metadata=metadata)
