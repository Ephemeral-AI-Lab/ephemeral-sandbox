"""Background task manager for async tool execution."""

from __future__ import annotations

import asyncio
import logging
import time
from collections.abc import Callable, Coroutine, Iterator
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any

from tools import ToolResult
from message.stream_events import BackgroundTaskStarted

logger = logging.getLogger(__name__)

# Async callback that physically kills the sandbox process.
KillCallback = Callable[[], Coroutine[Any, Any, None]]


class TaskStatus(StrEnum):
    """Lifecycle states for a tracked background task.

    Transitions:
        RUNNING -> {COMPLETED, FAILED, CANCELLED} -> DELIVERED

    Only :meth:`BackgroundTaskManager.collect_completed` advances a task
    from a terminal state (COMPLETED/FAILED/CANCELLED) to DELIVERED.
    """

    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    DELIVERED = "delivered"


# Terminal states that are still "undelivered" and waiting for the engine
# to pick them up via :meth:`BackgroundTaskManager.collect_completed`.
_TERMINAL_UNDELIVERED: frozenset[TaskStatus] = frozenset(
    {TaskStatus.COMPLETED, TaskStatus.FAILED, TaskStatus.CANCELLED}
)


@dataclass
class TrackedBackgroundTask:
    """A background task tracked by the manager."""

    task_id: str
    tool_name: str
    tool_input: dict[str, Any]
    asyncio_task: asyncio.Task[ToolResult]
    # Discriminator so monitoring/UI/audit can branch without sniffing tool_name.
    # "agent" for ordinary background tools, "subagent" for run_subagent.
    task_type: str = "agent"
    # Optional back-reference to a persisted AgentRunRecord (set by run_subagent
    # so the audit row and the in-memory bg task can be cross-resolved).
    agent_run_id: str | None = None
    status: TaskStatus = TaskStatus.RUNNING
    # Reason captured by cancel(); kept on the tracked task so callers (and
    # the subagent finaliser) can persist it to the audit record.
    cancel_reason: str | None = None
    # Cancellation / stop mode requested by the manager. Ordinary tools use
    # "cancel"; subagents may use "early_stop" so the task can salvage a
    # partial result before reaching a terminal state.
    stop_mode: str | None = None
    # Final completion flavor for successful-but-interrupted tasks.
    completion_mode: str | None = None
    result: ToolResult | None = None
    started_at: float = field(default_factory=time.monotonic)
    progress_lines: list[str] = field(default_factory=list)
    kill_callback: KillCallback | None = None  # physically kills the sandbox process
    # Optional pull-callback that returns a fresh progress snapshot on demand.
    # Used by tools (e.g. run_subagent) that have structured progress state
    # which is more meaningful than a flat line buffer. When set, get_status
    # calls this instead of joining progress_lines for running tasks.
    progress_provider: Callable[[int], str] | None = None


class BackgroundTaskManager:
    """Manages async background tasks launched by the query loop.

    This is dumb plumbing -- no error detection, no auto-cancel, no alerts.
    The LLM is the decision-maker.
    """

    def __init__(self) -> None:
        self._tasks: dict[str, TrackedBackgroundTask] = {}
        self._alias_counter: int = 0

    def next_alias(self) -> str:
        """Return a short mnemonic task_id like 'bg_1', 'bg_2', ...

        These are easier for the LLM to retain in tool outputs than opaque
        tool_use_ids and are what the agent sees as ``task_id`` everywhere.
        """
        self._alias_counter += 1
        return f"bg_{self._alias_counter}"

    def launch(
        self,
        task_id: str,
        tool_name: str,
        tool_input: dict[str, Any],
        coro: Coroutine[Any, Any, ToolResult],
        kill_callback: KillCallback | None = None,
        task_type: str = "agent",
        agent_run_id: str | None = None,
    ) -> BackgroundTaskStarted:
        """Launch *coro* as a background task and return a started event."""
        asyncio_task = asyncio.create_task(coro)
        tracked = TrackedBackgroundTask(
            task_id=task_id,
            tool_name=tool_name,
            tool_input=tool_input,
            asyncio_task=asyncio_task,
            task_type=task_type,
            agent_run_id=agent_run_id,
            kill_callback=kill_callback,
        )
        start_line = f"[started: {tool_name}]"
        tracked.progress_lines.append(start_line)
        self._tasks[task_id] = tracked

        def _done_callback(task: asyncio.Task[ToolResult]) -> None:
            # If cancel() already marked this task, don't overwrite its
            # status/result — the SDK may complete normally with exit_code -1
            # after we logically cancelled it.
            if tracked.status in (TaskStatus.CANCELLED, TaskStatus.DELIVERED):
                if task.cancelled():
                    logger.debug(
                        "Background task %s observed asyncio cancellation after cancel",
                        tracked.task_id,
                    )
                elif task.exception() is not None:
                    logger.debug(
                        "Background task %s raised after cancel: %s",
                        tracked.task_id,
                        task.exception(),
                    )
                return
            try:
                if task.cancelled():
                    tracked.status = TaskStatus.CANCELLED
                    tracked.result = ToolResult(output="Cancelled", is_error=True)
                elif task.exception() is not None:
                    exc = task.exception()
                    tracked.status = TaskStatus.FAILED
                    tracked.result = ToolResult(output=str(exc), is_error=True)
                else:
                    tracked.status = TaskStatus.COMPLETED
                    if tracked.stop_mode == "early_stop":
                        tracked.completion_mode = "early_stopped"
                    tracked.result = task.result()
            except Exception as exc:
                logger.debug("done_callback failed for %s: %s", tracked.task_id, exc)
                tracked.status = TaskStatus.FAILED
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
        returned again. This is the *only* method that performs the
        terminal → delivered transition.
        """
        ready: list[TrackedBackgroundTask] = []
        for tracked in self._tasks.values():
            if tracked.status in _TERMINAL_UNDELIVERED:
                tracked.status = TaskStatus.DELIVERED
                ready.append(tracked)
        return ready

    def iter_all(self) -> Iterator[TrackedBackgroundTask]:
        """Iterate every task the manager has ever tracked."""
        return iter(self._tasks.values())

    def iter_running(self) -> Iterator[TrackedBackgroundTask]:
        """Iterate tasks that are still running."""
        return (t for t in self._tasks.values() if t.status == TaskStatus.RUNNING)

    def has_pending(self) -> bool:
        """Return True if any task is still running."""
        return any(t.status == TaskStatus.RUNNING for t in self._tasks.values())

    async def wait_for(self, task_id: str, timeout: float) -> TrackedBackgroundTask | None:
        """Wait for a *specific* task to complete or *timeout* expires.

        Unlike :meth:`wait_any`, this does NOT call :meth:`collect_completed`,
        so completion events for *other* tasks are preserved for the engine's
        normal delivery path.
        """
        tracked = self._tasks.get(task_id)
        if tracked is None:
            return None
        if tracked.status != TaskStatus.RUNNING:
            return tracked
        try:
            await asyncio.wait(
                {tracked.asyncio_task},
                timeout=timeout,
                return_when=asyncio.FIRST_COMPLETED,
            )
        except Exception:
            return None
        return tracked if tracked.status != TaskStatus.RUNNING else None

    async def wait_any(self, timeout: float = 300) -> TrackedBackgroundTask | None:
        """Wait until any running task completes or *timeout* expires.

        Returns the first completed task, or ``None`` on timeout.
        Cost: zero tokens -- pure asyncio wait.
        """
        running = list(self.iter_running())
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

    def append_progress(self, task_id: str, line: str) -> None:
        """Append a live progress line for *task_id*.

        Used by streaming-capable tools to push incremental output into the
        manager so that ``check_background_task_result`` can return a live tail
        while the task is still running. Splits *line* on newlines so the
        caller can pass either a single line or a chunk of multiple lines.
        No-op if the task is unknown or already finished.
        """
        tracked = self._tasks.get(task_id)
        if tracked is None or tracked.status != TaskStatus.RUNNING:
            return
        for piece in str(line).splitlines() or [""]:
            tracked.progress_lines.append(piece)

    def set_progress_provider(self, task_id: str, provider: Callable[[int], str]) -> None:
        """Register a pull-callback for live progress on *task_id*.

        The provider is invoked synchronously by ``get_status`` while the task
        is still running. It should return a compact text snapshot of the
        task's current state. Errors raised by the provider are caught and
        surfaced as ``[progress provider error: ...]`` so a buggy provider
        can never crash the bg manager.
        """
        tracked = self._tasks.get(task_id)
        if tracked is not None:
            tracked.progress_provider = provider

    def make_progress_callback(self, task_id: str) -> Callable[[str], None]:
        """Return a callable that appends progress lines for *task_id*.

        Convenience for wiring into a tool's execution context — the tool
        can call ``ctx['on_progress_line']('hello')`` without ever
        knowing about the manager.
        """
        return lambda line: self.append_progress(task_id, line)

    def get_status(self, task_id: str | None = None, last_n: int = 20) -> list[dict[str, Any]]:
        """Return JSON-serializable status for tasks.

        If *task_id* is given, only that task is returned. Otherwise all
        tasks are included.
        """
        now = time.monotonic()
        result: list[dict[str, Any]] = []
        if task_id is not None:
            if task_id not in self._tasks:
                return []
            tasks: Any = [self._tasks[task_id]]
        else:
            tasks = self._tasks.values()
        for tracked in tasks:
            entry: dict[str, Any] = {
                "task_id": tracked.task_id,
                "tool_name": tracked.tool_name,
                "task_type": tracked.task_type,
                "agent_run_id": tracked.agent_run_id,
                "status": tracked.status,
                "elapsed_seconds": round(now - tracked.started_at, 1),
            }
            if tracked.cancel_reason:
                entry["cancel_reason"] = tracked.cancel_reason
            if tracked.stop_mode:
                entry["stop_mode"] = tracked.stop_mode
            if tracked.completion_mode:
                entry["completion_mode"] = tracked.completion_mode
            if tracked.result is not None:
                # Char-cap is applied by the tool layer (apply_last_n_lines)
                # AFTER line-tail trimming, so a long-tail run still yields
                # the requested number of trailing lines.
                entry["output"] = tracked.result.output
            elif tracked.status == TaskStatus.RUNNING:
                # Prefer the structured progress provider (e.g. run_subagent
                # returns a formatted view of its inner agent's last N
                # messages). Fall back to the line buffer for tools that
                # stream output via append_progress / on_progress_line.
                prefix = ""
                if tracked.stop_mode == "early_stop":
                    reason = f" ({tracked.cancel_reason})" if tracked.cancel_reason else ""
                    prefix = f"[early stop requested{reason}]\n"
                if tracked.progress_provider is not None:
                    try:
                        entry["output"] = prefix + tracked.progress_provider(last_n)
                    except Exception as exc:
                        entry["output"] = f"[progress provider error: {exc}]"
                elif tracked.progress_lines:
                    entry["output"] = prefix + "\n".join(tracked.progress_lines)
                else:
                    entry["output"] = prefix + "[no output captured yet]"
            result.append(entry)
        return result

    async def cancel(self, task_id: str, reason: str = "") -> bool:
        """Cancel a task by id. Returns True if found and cancelled.

        Marks the task as cancelled first, then attempts to physically
        kill the sandbox process via the kill_callback (if provided).
        We do NOT call asyncio.Task.cancel() for sandbox-backed work:
        sending CancelledError through an in-flight provider exec can corrupt
        the shared sandbox connection. Instead the kill_callback sends a kill
        signal to the sandbox process, letting the provider call return
        naturally.
        """
        tracked = self._tasks.get(task_id)
        if tracked is None:
            return False
        tracked.cancel_reason = reason or None
        if tracked.task_type == "subagent":
            tracked.stop_mode = "early_stop"
            tracked.progress_lines = [f"Early stop requested{': ' + reason if reason else ''}"]
            # Give a freshly launched subagent one event-loop cycle to reach its
            # first cooperative await so cancellation can be salvaged into a
            # partial result instead of short-circuiting before user code runs.
            await asyncio.sleep(0)
            tracked.asyncio_task.cancel()
            # Let trivial cancellation handlers and the task done-callback run
            # before we return status to the caller.
            await asyncio.sleep(0)
            return True
        tracked.stop_mode = "cancel"
        tracked.status = TaskStatus.CANCELLED
        msg = f"Cancelled: {reason}" if reason else "Cancelled"
        tracked.result = ToolResult(output=msg, is_error=True)
        tracked.progress_lines = [msg]
        if tracked.kill_callback is not None:
            try:
                await tracked.kill_callback()
            except Exception as exc:
                logger.debug("Kill callback failed for task %s: %s", task_id, exc)
        # Subagents may be inside async provider calls; logical cancel is enough.
        elif tracked.task_type != "subagent":
            # Pure-Python tools with no external runtime can be cancelled
            # cooperatively without risking the shared sandbox connection.
            tracked.asyncio_task.cancel()
        return True

    def get_task(self, task_id: str) -> TrackedBackgroundTask | None:
        """Return the tracked task for *task_id* (or None)."""
        return self._tasks.get(task_id)

    async def cancel_all(self) -> None:
        """Cancel all running tasks. Called on query loop exit."""
        cancelled_tasks: list[asyncio.Task[ToolResult]] = []
        for tracked in self._tasks.values():
            if tracked.status == TaskStatus.RUNNING:
                tracked.stop_mode = "cancel"
                tracked.status = TaskStatus.CANCELLED
                tracked.result = ToolResult(output="Cancelled", is_error=True)
                tracked.progress_lines = ["Cancelled"]
                if tracked.kill_callback is not None:
                    try:
                        await tracked.kill_callback()
                    except Exception:
                        # On query-loop shutdown this is the only hook that
                        # physically kills the sandbox process; a silently
                        # failed kill leaks a sandbox process and only
                        # surfaces hours later as "sandbox quota exhausted".
                        logger.warning(
                            "Kill callback failed for task %s",
                            tracked.task_id,
                            exc_info=True,
                        )
                # Subagents may be inside async provider calls; logical cancel is enough.
                elif tracked.task_type != "subagent":
                    tracked.asyncio_task.cancel()
                    cancelled_tasks.append(tracked.asyncio_task)
        if cancelled_tasks:
            await asyncio.gather(*cancelled_tasks, return_exceptions=True)
