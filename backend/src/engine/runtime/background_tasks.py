"""Background task manager for async tool execution."""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from typing import Any, Coroutine

from tools.core.base import ToolResult
from message.stream_events import BackgroundTaskStarted


@dataclass
class TrackedBackgroundTask:
    """A background task tracked by the manager."""

    task_id: str
    tool_name: str
    tool_input: dict[str, Any]
    asyncio_task: asyncio.Task[ToolResult]
    task_note: str = ""  # LLM-generated brief description of what the task does
    status: str = "running"  # running, completed, failed, cancelled, delivered
    result: ToolResult | None = None
    started_at: float = field(default_factory=time.monotonic)
    progress_lines: list[str] = field(default_factory=list)
    _last_reminder_line_idx: int = 0  # tracks where the last reminder left off
    _last_reminder_at: float = 0.0  # monotonic time of last reminder


class BackgroundTaskManager:
    """Manages async background tasks launched by the query loop.

    This is dumb plumbing -- no error detection, no auto-cancel, no alerts.
    The LLM is the decision-maker.
    """

    def __init__(self) -> None:
        self._tasks: dict[str, TrackedBackgroundTask] = {}

    def launch(
        self,
        task_id: str,
        tool_name: str,
        tool_input: dict[str, Any],
        coro: Coroutine[Any, Any, ToolResult],
        task_note: str = "",
    ) -> BackgroundTaskStarted:
        """Launch *coro* as a background task and return a started event."""
        asyncio_task = asyncio.create_task(coro)
        tracked = TrackedBackgroundTask(
            task_id=task_id,
            tool_name=tool_name,
            tool_input=tool_input,
            asyncio_task=asyncio_task,
            task_note=task_note,
        )
        self._tasks[task_id] = tracked

        def _done_callback(task: asyncio.Task[ToolResult]) -> None:
            try:
                if task.cancelled():
                    tracked.status = "cancelled"
                    tracked.result = ToolResult(output="Cancelled", is_error=True)
                elif task.exception() is not None:
                    exc = task.exception()
                    tracked.status = "failed"
                    tracked.result = ToolResult(output=str(exc), is_error=True)
                else:
                    tracked.status = "completed"
                    tracked.result = task.result()
            except Exception:
                tracked.status = "failed"
                tracked.result = ToolResult(output="Unknown error in done callback", is_error=True)

            # Populate progress_lines from the final result.
            if tracked.result is not None and tracked.result.output:
                tracked.progress_lines = tracked.result.output.splitlines()

        asyncio_task.add_done_callback(_done_callback)

        return BackgroundTaskStarted(
            task_id=task_id,
            tool_name=tool_name,
            tool_input=tool_input,
        )

    def collect_completed(self) -> list[TrackedBackgroundTask]:
        """Return tasks that finished but haven't been delivered yet.

        Each returned task is marked as ``delivered`` so it won't be
        returned again.
        """
        ready: list[TrackedBackgroundTask] = []
        for tracked in self._tasks.values():
            if tracked.status in ("completed", "failed", "cancelled"):
                tracked.status = "delivered"
                ready.append(tracked)
        return ready

    def has_pending(self) -> bool:
        """Return True if any task is still running."""
        return any(t.status == "running" for t in self._tasks.values())

    async def wait_any(self, timeout: float = 300) -> TrackedBackgroundTask | None:
        """Wait until any running task completes or *timeout* expires.

        Returns the first completed task, or ``None`` on timeout.
        Cost: zero tokens -- pure asyncio wait.
        """
        running = [t for t in self._tasks.values() if t.status == "running"]
        if not running:
            return None

        # Check if any are already done (callback fired between our check
        # and now).
        for tracked in running:
            if tracked.asyncio_task.done():
                # The done callback already ran; collect it.
                completed = self.collect_completed()
                return completed[0] if completed else None

        aws = [t.asyncio_task for t in running]
        try:
            done, _ = await asyncio.wait(aws, timeout=timeout, return_when=asyncio.FIRST_COMPLETED)
        except Exception:
            return None

        if not done:
            return None

        # The done callback has already fired for tasks in *done*.
        completed = self.collect_completed()
        return completed[0] if completed else None

    def compact_status(self, max_lines_per_task: int = 5, /) -> str:
        """Return a compact human-readable summary of all tasks."""
        if not self._tasks:
            return "[BACKGROUND TASKS STATUS]\nNo background tasks."

        now = time.monotonic()
        lines = ["[BACKGROUND TASKS STATUS]"]
        for tracked in self._tasks.values():
            elapsed = now - tracked.started_at
            label = tracked.task_note or tracked.tool_name
            line = (
                f"- {label} (task_id: {tracked.task_id}, {elapsed:.0f}s elapsed): {tracked.status}"
            )
            lines.append(line)
            if tracked.progress_lines:
                tail = tracked.progress_lines[-max_lines_per_task:]
                for pl in tail:
                    lines.append(f"  {pl}")
        return "\n".join(lines)

    def get_status(self, task_id: str | None = None) -> list[dict[str, Any]]:
        """Return JSON-serializable status for tasks.

        If *task_id* is given, only that task is returned. Otherwise all
        tasks are included.
        """
        now = time.monotonic()
        result: list[dict[str, Any]] = []
        tasks = (
            [self._tasks[task_id]] if task_id and task_id in self._tasks else self._tasks.values()
        )
        for tracked in tasks:
            entry: dict[str, Any] = {
                "task_id": tracked.task_id,
                "task_note": tracked.task_note,
                "tool_name": tracked.tool_name,
                "status": tracked.status,
                "elapsed_seconds": round(now - tracked.started_at, 1),
            }
            if tracked.result is not None:
                output = tracked.result.output
                if len(output) > 2000:
                    output = output[:2000] + "... (truncated)"
                entry["output"] = output
            result.append(entry)
        return result

    def cancel(self, task_id: str, reason: str = "") -> bool:
        """Cancel a task by id. Returns True if found and cancelled."""
        tracked = self._tasks.get(task_id)
        if tracked is None:
            return False
        tracked.asyncio_task.cancel()
        tracked.status = "cancelled"
        msg = f"Cancelled: {reason}" if reason else "Cancelled"
        tracked.result = ToolResult(output=msg, is_error=True)
        tracked.progress_lines = [msg]
        return True

    def get_reminder_diff(self, task_id: str, max_lines: int = 10) -> tuple[list[str], float]:
        """Return new progress lines since the last reminder for *task_id*.

        Advances the internal cursor so the next call returns only newer lines.
        Returns ``(new_lines, seconds_since_last_reminder)``.
        """
        tracked = self._tasks.get(task_id)
        if tracked is None:
            return [], 0.0
        now = time.monotonic()
        since = (
            now - tracked._last_reminder_at
            if tracked._last_reminder_at
            else now - tracked.started_at
        )
        new_lines = tracked.progress_lines[tracked._last_reminder_line_idx :]
        tracked._last_reminder_line_idx = len(tracked.progress_lines)
        tracked._last_reminder_at = now
        if len(new_lines) > max_lines:
            new_lines = new_lines[-max_lines:]
        return new_lines, since

    def cancel_all(self) -> None:
        """Cancel all running tasks. Called on query loop exit."""
        for tracked in self._tasks.values():
            if tracked.status == "running":
                tracked.asyncio_task.cancel()
                tracked.status = "cancelled"
                tracked.result = ToolResult(output="Cancelled", is_error=True)
                tracked.progress_lines = ["Cancelled"]
