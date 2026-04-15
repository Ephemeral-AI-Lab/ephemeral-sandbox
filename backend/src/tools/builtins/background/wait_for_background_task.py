"""Built-in tool for blocking until background tasks complete."""

from __future__ import annotations

import time
from typing import Any

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, ToolExecutionContext, ToolResult

from ._common import (
    TASK_ID_FIELD,
    apply_last_n_lines,
    build_background_snapshot_metadata,
    render_background_snapshot,
)

_FRESH_SUBAGENT_WAIT_SECONDS = 2.0


def _is_fresh_running_subagent(status: dict[str, Any]) -> bool:
    return (
        status.get("status") == "running"
        and status.get("task_type") == "subagent"
        and float(status.get("elapsed_seconds") or 0.0) < _FRESH_SUBAGENT_WAIT_SECONDS
    )


def _needs_progress_check(status: dict[str, Any], manager: Any) -> bool:
    if status.get("status") != "running" or status.get("task_type") != "subagent":
        return False
    task_id = str(status.get("task_id") or "").strip()
    tracked = manager.get_task(task_id) if task_id else None
    if tracked is None:
        return False
    return int(getattr(tracked, "progress_checks", 0) or 0) < 1


def _fresh_wait_rejection(task_ids: list[str]) -> ToolResult:
    joined = ", ".join(task_ids)
    return ToolResult(
        output=(
            "[WAIT_TOO_EARLY] The requested background join targets a freshly launched "
            f"subagent ({joined}) that has not had time to produce useful progress yet. "
            "Do not treat run_subagent like a foreground call. Keep working other ready "
            "branches or use check_background_progress first, then wait only after the "
            "subagent has produced meaningful progress or becomes the sole blocker."
        ),
        is_error=True,
    )


def _unchecked_wait_rejection(task_ids: list[str]) -> ToolResult:
    joined = ", ".join(task_ids)
    suggestion = (
        'check_background_progress(task_id="all")'
        if len(task_ids) > 1
        else f'check_background_progress(task_id="{task_ids[0]}")'
    )
    return ToolResult(
        output=(
            "[WAIT_REQUIRES_PROGRESS_CHECK] The requested background join targets "
            f"subagent task(s) ({joined}) that you have not inspected yet. "
            f"Call {suggestion} first. Engine reminders or streamed progress snippets "
            "do not count as that inspection. Then keep working other ready branches "
            "or wait only once the inspected subagent is the remaining blocker."
        ),
        is_error=True,
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
            unchecked_running = [
                s for s in snapshot
                if _needs_progress_check(s, manager)
            ]
            if unchecked_running:
                return _unchecked_wait_rejection(
                    [str(s.get("task_id") or "") for s in unchecked_running if s.get("task_id")]
                )
            fresh_running = [
                s for s in snapshot
                if _is_fresh_running_subagent(s)
            ]
            if fresh_running and len(fresh_running) == len(
                [s for s in snapshot if s.get("status") == "running"]
            ):
                return _fresh_wait_rejection(
                    [str(s.get("task_id") or "") for s in fresh_running if s.get("task_id")]
                )
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
            if _needs_progress_check(task_statuses[0], manager):
                return _unchecked_wait_rejection([target_id])
            if _is_fresh_running_subagent(task_statuses[0]):
                return _fresh_wait_rejection([target_id])
            if task_statuses[0].get("status") != "running":
                apply_last_n_lines(task_statuses, arguments.last_n_lines)
                notice = (
                    f"[ALREADY_COMPLETED] Task {target_id} had already finished "
                    "before this wait call was issued — no waiting occurred. "
                    "Your assumption that it was still running is stale; update "
                    "your mental model from the status payload below."
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

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True
