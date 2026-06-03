"""Background task lifecycle supervision for async tool execution."""

from __future__ import annotations

import asyncio
import logging
import os
import threading
import time
from collections.abc import Callable, Coroutine, Iterator
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any

from sandbox.audit.schema import (
    BackgroundToolSection,
    build_background_tool_event,
    safe_emit,
)
from tools import ToolResult
from message.events import BackgroundTaskStartedEvent

logger = logging.getLogger(__name__)
_HEARTBEAT_INTERVAL_S = float(os.environ.get("EOS_BACKGROUND_HEARTBEAT_INTERVAL_S", "60"))
DEFAULT_BACKGROUND_TASK_TYPE = "agent"
SUBAGENT_TASK_TYPE = "subagent"
WORKFLOW_TASK_TYPE = "workflow"
_EARLY_STOP_MODE = "early_stop"
_EARLY_STOP_COMPLETION_MODE = "early_stopped"
_PARENT_EXIT_STOP_MODE = "parent_exit"
_PARENT_EXIT_REASON = "non_cancellation_tool_request"


class BackgroundTaskStatus(StrEnum):
    """Lifecycle states for a tracked background task.

    Transitions:
        RUNNING -> {COMPLETED, FAILED, CANCELLED} -> DELIVERED

    Terminal records advance to DELIVERED through their typed delivery path.
    """

    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    DELIVERED = "delivered"


# Terminal status precedence used by
# :meth:`BackgroundTaskSupervisor._apply_terminal_status_transition`.
# A status with a *higher* precedence overwrites a lower one; otherwise the
# attempt is dropped. Cancel + natural-completion races resolve to COMPLETED so
# a long-running task or command session that finishes between cancel and reap
# returns its real result, not a late cancellation result.
_TERMINAL_PRECEDENCE: dict[BackgroundTaskStatus, int] = {
    BackgroundTaskStatus.RUNNING: 0,
    BackgroundTaskStatus.CANCELLED: 1,
    BackgroundTaskStatus.FAILED: 2,
    BackgroundTaskStatus.COMPLETED: 3,
    BackgroundTaskStatus.DELIVERED: 4,
}


# Terminal states that are still "undelivered" and waiting for a typed delivery
# path to pick them up.
_TERMINAL_UNDELIVERED: frozenset[BackgroundTaskStatus] = frozenset(
    {
        BackgroundTaskStatus.COMPLETED,
        BackgroundTaskStatus.FAILED,
        BackgroundTaskStatus.CANCELLED,
    }
)


@dataclass
class BackgroundTaskRecord:
    """In-memory record for one engine-owned background task."""

    task_id: str
    tool_name: str
    tool_input: dict[str, Any]
    asyncio_task: asyncio.Task[ToolResult]
    # Discriminator so monitoring/UI/audit can branch without sniffing tool_name.
    # "subagent" is the model-facing engine background task type.
    task_type: str = DEFAULT_BACKGROUND_TASK_TYPE
    subagent_session_id: str | None = None
    agent_id: str | None = None
    uses_sandbox: bool = False
    sandbox_id: str | None = None
    sandbox_invocation_id: str | None = None
    heartbeat_enabled: bool = True
    status: BackgroundTaskStatus = BackgroundTaskStatus.RUNNING
    # Reason captured by cancel(); kept on the tracked task so callers (and
    # the subagent finaliser) can persist it to the audit record.
    cancel_reason: str | None = None
    # Cancellation / stop mode requested by the supervisor. Ordinary tools use
    # "cancel"; subagents may use "early_stop" so the task can salvage a
    # partial result before reaching a terminal state.
    stop_mode: str | None = None
    # Final completion flavor for successful-but-interrupted tasks.
    completion_mode: str | None = None
    result: ToolResult | None = None
    started_at: float = field(default_factory=time.monotonic)
    progress_lines: list[str] = field(default_factory=list)
    # Optional pull-callback that returns a fresh progress snapshot on demand.
    # Used by tools (e.g. run_subagent) that have structured progress state
    # which is more meaningful than a flat line buffer.
    progress_provider: Callable[[int], str] | None = None
    # Single-writer latch around the status/result mutation. The cancel path
    # and the asyncio done-callback can both race to set a terminal status;
    # the lock + ``_TERMINAL_PRECEDENCE`` table make that race deterministic.
    _terminal_lock: threading.Lock = field(default_factory=threading.Lock)


@dataclass
class CommandSessionRecord:
    """Lightweight supervision state for a daemon-owned command session."""

    command_session_id: str
    sandbox_id: str
    agent_id: str
    command: str = ""
    status: BackgroundTaskStatus = BackgroundTaskStatus.RUNNING
    result: dict[str, Any] | None = None
    started_at: float = field(default_factory=time.monotonic)


@dataclass
class WorkflowBackgroundRecord:
    """Lightweight supervision state for one delegated workflow handle."""

    workflow_task_id: str
    workflow_id: str
    parent_task_id: str
    parent_attempt_id: str | None
    request_id: str
    agent_id: str
    goal: str
    status: BackgroundTaskStatus = BackgroundTaskStatus.RUNNING
    final_status: str | None = None
    final_outcomes: list[dict[str, Any]] = field(default_factory=list)
    terminal_reported_by_status_tool: bool = False
    terminal_reported_by_notification: bool = False
    cancelled_by_cancel_tool: bool = False
    parent_cancelled: bool = False
    started_at: float = field(default_factory=time.monotonic)
    last_seen_at: float = field(default_factory=time.monotonic)

    @property
    def delivered(self) -> bool:
        return (
            self.status == BackgroundTaskStatus.DELIVERED
            or self.terminal_reported_by_status_tool
            or self.terminal_reported_by_notification
            or self.cancelled_by_cancel_tool
        )

    @property
    def outstanding(self) -> bool:
        return self.status == BackgroundTaskStatus.RUNNING or (
            self.status in _TERMINAL_UNDELIVERED and not self.delivered
        )


_TERMINAL_EVENT_TYPE: dict[str, str] = {
    "completed": "background_tool.completed",
    "failed": "background_tool.failed",
    "cancelled": "background_tool.cancelled",
}


def _emit_background_tool(
    event_type: str,
    tracked: BackgroundTaskRecord,
    *,
    lane: str = "normal",
    duration_ms: float | None = None,
    delivery_latency_ms: float | None = None,
    uptime_ms: float | None = None,
) -> None:
    """Emit one ``background_tool.*`` event into the daemon ring."""
    result = tracked.result
    safe_emit(
        build_background_tool_event(
            event_type,
            BackgroundToolSection(
                background_task_id=tracked.task_id,
                task_kind=tracked.task_type,
                tool_name=tracked.tool_name,
                agent_id=tracked.agent_id,
                uptime_ms=uptime_ms,
                status=tracked.status.value,
                exit_code=(
                    0
                    if result is not None and not result.is_error
                    else (1 if result is not None else None)
                ),
                duration_ms=duration_ms,
                error_kind=(
                    "error"
                    if result is not None and result.is_error
                    else None
                ),
                cancel_reason=tracked.cancel_reason,
                delivery_latency_ms=delivery_latency_ms,
            ),
        ),
        lane=lane,  # type: ignore[arg-type]
    )


def _running_sandbox_task(
    tracked: BackgroundTaskRecord,
    agent_id: str | None = None,
) -> bool:
    if tracked.status != BackgroundTaskStatus.RUNNING or not tracked.uses_sandbox:
        return False
    return agent_id is None or tracked.agent_id == agent_id


async def _request_subagent_early_stop(
    tracked: BackgroundTaskRecord,
    *,
    reason: str = "",
) -> None:
    tracked.stop_mode = _EARLY_STOP_MODE
    tracked.progress_lines = [f"Early stop requested{': ' + reason if reason else ''}"]
    # Give a freshly launched subagent one event-loop cycle to reach its first
    # cooperative await so cancellation can be salvaged into a partial result.
    await asyncio.sleep(0)
    tracked.asyncio_task.cancel()
    # Let trivial cancellation handlers and the task done-callback run before
    # status is reported back to the caller.
    await asyncio.sleep(0)


def _parent_exit_result(reason: str) -> ToolResult:
    return ToolResult(
        output=f"Terminated by parent exit: {reason}",
        is_error=True,
        metadata={
            "subagent_terminal_called": False,
            "subagent_termination_reason": reason,
        },
    )


def _cancelled_result(tracked: BackgroundTaskRecord, message: str) -> ToolResult:
    metadata: dict[str, Any] = {}
    if tracked.task_type == SUBAGENT_TASK_TYPE:
        metadata = {
            "subagent_terminal_called": False,
            "subagent_cancelled": True,
        }
    return ToolResult(output=message, is_error=True, metadata=metadata)


class BackgroundTaskSupervisor:
    """Supervise async background tasks launched by the query loop.

    This is dumb plumbing: no error detection, no auto-cancel, no alerts.
    The LLM is the decision-maker.
    """

    def __init__(self) -> None:
        self._tasks: dict[str, BackgroundTaskRecord] = {}
        self._command_sessions: dict[str, CommandSessionRecord] = {}
        self._workflows: dict[str, WorkflowBackgroundRecord] = {}
        self._alias_counter: int = 0
        self._subagent_counter: int = 0
        self._workflow_counter: int = 0
        self._heartbeat_task: asyncio.Task[None] | None = None

    def next_alias(self) -> str:
        """Return a short mnemonic task id for internal supervisor tests."""
        self._alias_counter += 1
        return f"bg_{self._alias_counter}"

    def next_subagent_session_id(self) -> str:
        """Return the single supervisor/model id for one subagent session."""
        self._subagent_counter += 1
        return f"subagent_{self._subagent_counter}"

    def next_workflow_task_id(self) -> str:
        """Return the agent-facing id for one delegated workflow."""
        self._workflow_counter += 1
        return f"wf_{self._workflow_counter}"

    def launch(
        self,
        task_id: str,
        tool_name: str,
        tool_input: dict[str, Any],
        coro: Coroutine[Any, Any, ToolResult],
        task_type: str = DEFAULT_BACKGROUND_TASK_TYPE,
        agent_id: str | None = None,
        uses_sandbox: bool = False,
        sandbox_id: str | None = None,
        sandbox_invocation_id: str | None = None,
        heartbeat_enabled: bool = True,
        subagent_session_id: str | None = None,
    ) -> BackgroundTaskStartedEvent:
        """Launch *coro* as a background task and return a started event."""
        record_id = task_id
        if task_type == SUBAGENT_TASK_TYPE:
            record_id = subagent_session_id or task_id
        if task_type == SUBAGENT_TASK_TYPE:
            subagent_session_id = record_id
        asyncio_task = asyncio.create_task(coro)
        tracked = BackgroundTaskRecord(
            task_id=record_id,
            tool_name=tool_name,
            tool_input=tool_input,
            asyncio_task=asyncio_task,
            task_type=task_type,
            subagent_session_id=subagent_session_id,
            agent_id=agent_id,
            uses_sandbox=uses_sandbox,
            sandbox_id=sandbox_id,
            sandbox_invocation_id=sandbox_invocation_id,
            heartbeat_enabled=heartbeat_enabled,
        )
        start_line = f"[started: {tool_name}]"
        tracked.progress_lines.append(start_line)
        self._tasks[record_id] = tracked
        _emit_background_tool("background_tool.started", tracked)

        def _done_callback(task: asyncio.Task[ToolResult]) -> None:
            try:
                if task.cancelled():
                    result = (
                        _parent_exit_result(tracked.cancel_reason or _PARENT_EXIT_REASON)
                        if tracked.stop_mode == _PARENT_EXIT_STOP_MODE
                        else _cancelled_result(tracked, "Cancelled")
                    )
                    self._apply_terminal_status_transition(
                        tracked,
                        new_status=BackgroundTaskStatus.CANCELLED,
                        new_result=result,
                    )
                elif task.exception() is not None:
                    exc = task.exception()
                    self._apply_terminal_status_transition(
                        tracked,
                        new_status=BackgroundTaskStatus.FAILED,
                        new_result=ToolResult(output=str(exc), is_error=True),
                    )
                else:
                    real_result = task.result()
                    if tracked.stop_mode == _PARENT_EXIT_STOP_MODE:
                        self._apply_terminal_status_transition(
                            tracked,
                            new_status=BackgroundTaskStatus.CANCELLED,
                            new_result=_parent_exit_result(
                                tracked.cancel_reason or _PARENT_EXIT_REASON
                            ),
                        )
                        return
                    applied = self._apply_terminal_status_transition(
                        tracked,
                        new_status=BackgroundTaskStatus.COMPLETED,
                        new_result=real_result,
                    )
                    if applied:
                        if tracked.stop_mode == _EARLY_STOP_MODE:
                            tracked.completion_mode = _EARLY_STOP_COMPLETION_MODE
            except Exception as exc:
                logger.debug("done_callback failed for %s: %s", tracked.task_id, exc)
                self._apply_terminal_status_transition(
                    tracked,
                    new_status=BackgroundTaskStatus.FAILED,
                    new_result=ToolResult(
                        output="Unknown error in done callback",
                        is_error=True,
                    ),
                )

            # Populate progress_lines from whichever result the latch settled on.
            if tracked.result is not None and tracked.result.output:
                tracked.progress_lines = tracked.result.output.splitlines()
            self._stop_heartbeat_if_idle()

        asyncio_task.add_done_callback(_done_callback)
        if (
            tracked.uses_sandbox
            and tracked.sandbox_invocation_id
            and tracked.sandbox_id
            and tracked.heartbeat_enabled
        ):
            self._ensure_heartbeat_task()

        return BackgroundTaskStartedEvent(
            task_id=record_id,
            tool_name=tool_name,
            tool_input=tool_input,
        )

    def collect_completed(self) -> list[BackgroundTaskRecord]:
        """Return non-subagent tasks that finished but haven't been delivered yet.

        Each returned task is marked as ``delivered`` so it won't be
        returned again. Subagents use their own typed completion/progress
        delivery path and are intentionally skipped here.
        """
        ready: list[BackgroundTaskRecord] = []
        for tracked in self._tasks.values():
            if (
                tracked.task_type == SUBAGENT_TASK_TYPE
                or tracked.status not in _TERMINAL_UNDELIVERED
            ):
                continue
            self._mark_delivered(tracked)
            ready.append(tracked)
        return ready

    def iter_all(self) -> Iterator[BackgroundTaskRecord]:
        """Iterate every task the supervisor has ever tracked."""
        return iter(self._tasks.values())

    def iter_workflows(self) -> Iterator[WorkflowBackgroundRecord]:
        """Iterate delegated workflow handle records."""
        return iter(self._workflows.values())

    def iter_running(self) -> Iterator[BackgroundTaskRecord]:
        """Iterate tasks that are still running."""
        return (
            task
            for task in self._tasks.values()
            if task.status == BackgroundTaskStatus.RUNNING
        )

    def has_pending(self) -> bool:
        """Return True if any task is still running."""
        return any(
            task.status == BackgroundTaskStatus.RUNNING for task in self._tasks.values()
        ) or bool(self._running_command_sessions())

    def count_by_agent(self, agent_id: str) -> int:
        """Return running sandbox-bound background task count for one agent."""
        task_count = sum(
            1
            for tracked in self._tasks.values()
            if _running_sandbox_task(tracked, agent_id)
        )
        command_session_count = sum(
            1
            for tracked in self._command_sessions.values()
            if tracked.status == BackgroundTaskStatus.RUNNING
            and tracked.agent_id == agent_id
        )
        workflow_count = sum(
            1
            for tracked in self._workflows.values()
            if tracked.agent_id == agent_id and tracked.outstanding
        )
        return task_count + command_session_count + workflow_count

    def register_workflow(
        self,
        *,
        workflow_id: str,
        parent_task_id: str,
        parent_attempt_id: str | None,
        request_id: str,
        agent_id: str,
        goal: str,
    ) -> WorkflowBackgroundRecord:
        """Track a delegated workflow handle owned by one parent agent."""
        existing = self.find_outstanding_workflow_for_parent(
            parent_task_id=parent_task_id,
            agent_id=agent_id,
        )
        if existing is not None:
            return existing
        workflow_task_id = self.next_workflow_task_id()
        record = WorkflowBackgroundRecord(
            workflow_task_id=workflow_task_id,
            workflow_id=workflow_id,
            parent_task_id=parent_task_id,
            parent_attempt_id=parent_attempt_id,
            request_id=request_id,
            agent_id=agent_id,
            goal=goal,
        )
        self._workflows[workflow_task_id] = record
        return record

    def find_outstanding_workflow_for_parent(
        self,
        *,
        parent_task_id: str,
        agent_id: str,
    ) -> WorkflowBackgroundRecord | None:
        """Return the first undelivered workflow for the parent task."""
        for record in self._workflows.values():
            if (
                record.parent_task_id == parent_task_id
                and record.agent_id == agent_id
                and record.outstanding
            ):
                return record
        return None

    def find_workflow_record(
        self,
        *,
        workflow_id: str,
        workflow_task_id: str | None = None,
        agent_id: str | None = None,
    ) -> WorkflowBackgroundRecord | None:
        """Resolve a delegated workflow handle by exact id or workflow id."""
        if workflow_task_id:
            record = self._workflows.get(workflow_task_id)
            if record is None:
                return None
            if workflow_id and record.workflow_id != workflow_id:
                return None
            if agent_id is not None and record.agent_id != agent_id:
                return None
            return record
        matches = [
            record
            for record in self._workflows.values()
            if record.workflow_id == workflow_id
            and (agent_id is None or record.agent_id == agent_id)
        ]
        if len(matches) == 1:
            return matches[0]
        return None

    def refresh_workflow_status(
        self,
        *,
        workflow_id: str,
        status: str,
        outcomes: list[dict[str, Any]] | None = None,
    ) -> None:
        """Refresh all records for a workflow from durable workflow state."""
        terminal_status = {
            "succeeded": BackgroundTaskStatus.COMPLETED,
            "failed": BackgroundTaskStatus.FAILED,
            "cancelled": BackgroundTaskStatus.CANCELLED,
        }.get(status)
        if terminal_status is None:
            return
        for record in self._workflows.values():
            if record.workflow_id != workflow_id or record.status == BackgroundTaskStatus.DELIVERED:
                continue
            record.status = terminal_status
            record.final_status = status
            record.final_outcomes = list(outcomes or [])
            record.last_seen_at = time.monotonic()

    def mark_workflow_reported_by_status_tool(self, workflow_task_id: str) -> bool:
        """Mark terminal workflow outcomes delivered through an explicit status check."""
        record = self._workflows.get(workflow_task_id)
        if record is None or record.status not in _TERMINAL_UNDELIVERED:
            return False
        record.terminal_reported_by_status_tool = True
        record.status = BackgroundTaskStatus.DELIVERED
        record.last_seen_at = time.monotonic()
        return True

    def mark_workflow_cancelled_by_tool(
        self,
        *,
        workflow_task_id: str,
        reason: str = "",
    ) -> WorkflowBackgroundRecord | None:
        """Record an explicit model-requested workflow cancellation."""
        record = self._workflows.get(workflow_task_id)
        if record is None:
            return None
        record.cancelled_by_cancel_tool = True
        record.status = BackgroundTaskStatus.CANCELLED
        record.final_status = "cancelled"
        record.final_outcomes = [
            {
                "status": "failed",
                "role": "workflow",
                "task_id": workflow_task_id,
                "outcome": reason or "Delegated workflow was cancelled.",
            }
        ]
        record.last_seen_at = time.monotonic()
        return record

    def register_command_session(
        self,
        *,
        command_session_id: str,
        sandbox_id: str,
        agent_id: str,
        command: str = "",
    ) -> None:
        """Track a daemon-owned command session for lifecycle gates."""
        self._command_sessions[command_session_id] = CommandSessionRecord(
            command_session_id=command_session_id,
            sandbox_id=sandbox_id,
            agent_id=agent_id,
            command=command,
        )
        self._ensure_heartbeat_task()

    def mark_command_session_result_reported_by_tool(
        self,
        *,
        command_session_id: str,
        result: dict[str, Any],
    ) -> None:
        tracked = self._command_sessions.get(command_session_id)
        if tracked is None:
            return
        status = str(result.get("status") or "")
        if status == "running":
            return
        tracked.result = dict(result)
        tracked.status = BackgroundTaskStatus.DELIVERED
        self._stop_heartbeat_if_idle()

    def get_command_session_result(self, command_session_id: str) -> dict[str, Any] | None:
        """Return a stored terminal result, if notification polling claimed it."""
        tracked = self._command_sessions.get(command_session_id)
        if (
            tracked is None
            or tracked.result is None
            or tracked.status == BackgroundTaskStatus.RUNNING
        ):
            return None
        return dict(tracked.result)

    def append_progress(self, task_id: str, line: str) -> None:
        """Append a live progress line for *task_id*.

        Used by streaming-capable engine tasks to push incremental output into
        the supervisor. Splits *line* on newlines so the caller can pass either
        a single line or a chunk of multiple lines. No-op if the task is
        unknown or already finished.
        """
        tracked = self._tasks.get(task_id)
        if tracked is None or tracked.status != BackgroundTaskStatus.RUNNING:
            return
        for piece in str(line).splitlines() or [""]:
            tracked.progress_lines.append(piece)

    def set_progress_provider(self, task_id: str, provider: Callable[[int], str]) -> None:
        """Register a pull-callback for live progress on *task_id*.

        The provider is invoked synchronously by background result tools while
        the task is still running. It should return a compact text snapshot of
        the task's current state.
        """
        tracked = self._tasks.get(task_id)
        if tracked is not None:
            tracked.progress_provider = provider

    def make_progress_callback(self, task_id: str) -> Callable[[str], None]:
        """Return a callable that appends progress lines for *task_id*.

        Convenience for wiring into a tool's execution context — the tool
        can call ``ctx['on_progress_line']('hello')`` without ever
        knowing about the supervisor.
        """
        return lambda line: self.append_progress(task_id, line)

    async def cancel(self, task_id: str, reason: str = "") -> bool:
        """Cancel a task by id. Returns True if found and cancelled.

        Subagents receive a cooperative early-stop cancellation so they can
        salvage a partial result. Non-subagent supervisor records are cancelled
        through their asyncio task.

        Race-safe via the terminal-status latch: if the task already
        completed (e.g. a short command that exited just before the user clicked
        cancel), the COMPLETED result is preserved.
        """
        tracked = self._tasks.get(task_id)
        if tracked is None:
            return False
        tracked.cancel_reason = reason or None
        await self._cancel_sandbox_invocation_if_bound(tracked)
        if tracked.task_type == SUBAGENT_TASK_TYPE:
            await _request_subagent_early_stop(tracked, reason=reason)
            return True
        self._mark_cancelled(tracked, reason=reason)
        tracked.asyncio_task.cancel()
        self._stop_heartbeat_if_idle()
        return True

    async def cancel_by_agent(self, agent_id: str, *, grace_s: float) -> int:
        """Cancel running sandbox-bound background tasks for one agent.

        Returns the number of asyncio tasks still not done after ``grace_s``.
        """
        await self.cancel_command_sessions_by_agent(agent_id)
        targets = [
            tracked
            for tracked in self._tasks.values()
            if _running_sandbox_task(tracked, agent_id)
        ]
        if not targets:
            return 0
        await asyncio.gather(
            *(
                self.cancel(tracked.task_id, reason="isolated_workspace_exit")
                for tracked in targets
            ),
            return_exceptions=True,
        )
        pending = [
            tracked.asyncio_task
            for tracked in targets
            if not tracked.asyncio_task.done()
        ]
        if pending and grace_s > 0:
            _, still_pending = await asyncio.wait(pending, timeout=grace_s)
            pending = list(still_pending)
        for task in pending:
            task.cancel()
        return len([task for task in pending if not task.done()])

    async def cancel_command_sessions_by_agent(self, agent_id: str) -> int:
        """Cancel active command-session records for one agent."""
        targets = [
            record
            for record in self._command_sessions.values()
            if record.status == BackgroundTaskStatus.RUNNING
            and record.agent_id == agent_id
        ]
        if not targets:
            return 0
        try:
            import sandbox.api as sandbox_api

            await asyncio.gather(
                *(
                    sandbox_api.cancel_command_session(
                        record.sandbox_id,
                        sandbox_api.CommandSessionCancelRequest(
                            caller=sandbox_api.SandboxCaller(agent_id=record.agent_id),
                            command_session_id=record.command_session_id,
                        ),
                    )
                    for record in targets
                ),
                return_exceptions=True,
            )
        finally:
            for record in targets:
                record.status = BackgroundTaskStatus.CANCELLED
        return len(targets)

    async def collect_command_session_completion_notifications(self) -> list[str]:
        """Return one notification string for each newly terminal command session."""
        await self._collect_command_completions_once()
        notifications: list[str] = []
        for record in self._command_sessions.values():
            if record.status not in _TERMINAL_UNDELIVERED or record.result is None:
                continue
            notifications.append(_render_command_session_completion_notification(record))
            record.status = BackgroundTaskStatus.DELIVERED
        self._stop_heartbeat_if_idle()
        return notifications

    def get_task(self, task_id: str) -> BackgroundTaskRecord | None:
        """Return the tracked task for *task_id* (or None)."""
        return self._tasks.get(task_id)

    def get_subagent_task(self, subagent_session_id: str) -> BackgroundTaskRecord | None:
        """Return a subagent task by its session id."""
        direct = self._tasks.get(subagent_session_id)
        if direct is not None and direct.task_type == SUBAGENT_TASK_TYPE:
            return direct
        for tracked in self._tasks.values():
            if (
                tracked.task_type == SUBAGENT_TASK_TYPE
                and tracked.subagent_session_id == subagent_session_id
            ):
                return tracked
        return None

    async def cancel_subagent_session(
        self,
        subagent_session_id: str,
        reason: str = "",
    ) -> bool:
        """Cancel a running subagent by session id."""
        tracked = self.get_subagent_task(subagent_session_id)
        if tracked is None:
            return False
        return await self.cancel(tracked.task_id, reason)

    async def terminate_for_parent_exit(
        self,
        *,
        reason: str = _PARENT_EXIT_REASON,
        grace_s: float = 0.05,
    ) -> list[str]:
        """Terminate active subagents because the parent run is no longer continuing."""
        targets = [
            tracked
            for tracked in self._tasks.values()
            if tracked.task_type == SUBAGENT_TASK_TYPE
            and tracked.status == BackgroundTaskStatus.RUNNING
        ]
        if not targets:
            return []
        for tracked in targets:
            tracked.cancel_reason = reason
            tracked.stop_mode = _PARENT_EXIT_STOP_MODE
            result = _parent_exit_result(reason)
            applied = self._apply_terminal_status_transition(
                tracked,
                new_status=BackgroundTaskStatus.CANCELLED,
                new_result=result,
            )
            if applied:
                tracked.progress_lines = [result.output]
            tracked.asyncio_task.cancel()
        if grace_s >= 0:
            await asyncio.wait(
                [tracked.asyncio_task for tracked in targets],
                timeout=grace_s,
            )
        return self.collect_subagent_completion_notifications()

    def collect_subagent_completion_notifications(self) -> list[str]:
        """Return one notification string for each newly terminal subagent."""
        notifications: list[str] = []
        for tracked in self._tasks.values():
            if (
                tracked.task_type != SUBAGENT_TASK_TYPE
                or tracked.status not in _TERMINAL_UNDELIVERED
            ):
                continue
            notifications.append(_render_subagent_completion_notification(tracked))
            self._mark_delivered(tracked)
        return notifications

    def mark_subagent_delivered(self, subagent_session_id: str) -> bool:
        """Mark a terminal subagent delivered after an explicit progress check."""
        tracked = self.get_subagent_task(subagent_session_id)
        if tracked is None or tracked.status not in _TERMINAL_UNDELIVERED:
            return False
        self._mark_delivered(tracked)
        return True

    async def cancel_all(self) -> None:
        """Cancel all running tasks. Called on query loop exit."""
        command_session_agents = {
            record.agent_id for record in self._running_command_sessions()
        }
        await asyncio.gather(
            *(
                self.cancel_command_sessions_by_agent(agent_id)
                for agent_id in command_session_agents
            ),
            return_exceptions=True,
        )
        cancelled_tasks: list[asyncio.Task[ToolResult]] = []
        for tracked in self._tasks.values():
            if tracked.status != BackgroundTaskStatus.RUNNING:
                continue
            self._mark_cancelled(tracked)
            await self._cancel_sandbox_invocation_if_bound(tracked)
            if tracked.task_type != SUBAGENT_TASK_TYPE:
                tracked.asyncio_task.cancel()
                cancelled_tasks.append(tracked.asyncio_task)
        if cancelled_tasks:
            await asyncio.gather(*cancelled_tasks, return_exceptions=True)
        self._stop_heartbeat_if_idle()

    def _mark_cancelled(
        self,
        tracked: BackgroundTaskRecord,
        *,
        reason: str = "",
    ) -> None:
        tracked.stop_mode = "cancel"
        message = f"Cancelled: {reason}" if reason else "Cancelled"
        applied = self._apply_terminal_status_transition(
            tracked,
            new_status=BackgroundTaskStatus.CANCELLED,
            new_result=_cancelled_result(tracked, message),
        )
        if applied:
            tracked.progress_lines = [message]

    def _mark_delivered(self, tracked: BackgroundTaskRecord) -> None:
        tracked.status = BackgroundTaskStatus.DELIVERED
        delivery_latency_ms = max(
            0.0, (time.monotonic() - tracked.started_at) * 1000.0
        )
        _emit_background_tool(
            "background_tool.delivered",
            tracked,
            delivery_latency_ms=delivery_latency_ms,
        )

    def _apply_terminal_status_transition(
        self,
        tracked: BackgroundTaskRecord,
        *,
        new_status: BackgroundTaskStatus,
        new_result: ToolResult | None,
    ) -> bool:
        """CAS one terminal-status transition. Returns ``True`` if applied.

        Precedence: ``completed > failed > cancelled > running``. ``delivered``
        is the post-terminal sink; nothing overwrites it. The lock here is
        cheap (per-task, never contended outside of cancel races) and makes
        the precedence rule deterministic even if event-loop ordering
        re-shuffles cancel + done_callback.
        """
        new_rank = _TERMINAL_PRECEDENCE[new_status]
        with tracked._terminal_lock:
            current_rank = _TERMINAL_PRECEDENCE[tracked.status]
            if new_rank <= current_rank:
                return False
            tracked.status = new_status
            if new_result is not None:
                tracked.result = new_result
        event_type = _TERMINAL_EVENT_TYPE.get(new_status.value)
        if event_type is not None:
            duration_ms = max(0.0, (time.monotonic() - tracked.started_at) * 1000.0)
            _emit_background_tool(event_type, tracked, duration_ms=duration_ms)
        return True

    async def _cancel_sandbox_invocation_if_bound(
        self, tracked: BackgroundTaskRecord
    ) -> None:
        if (
            not tracked.uses_sandbox
            or not tracked.sandbox_id
            or not tracked.sandbox_invocation_id
        ):
            return
        try:
            import sandbox.api as sandbox_api

            await sandbox_api.cancel(tracked.sandbox_id, tracked.sandbox_invocation_id)
        except Exception as exc:
            logger.warning(
                "wire-cancel failed for task_id=%s invocation_id=%s: %s",
                tracked.task_id,
                tracked.sandbox_invocation_id,
                exc,
            )

    def _ensure_heartbeat_task(self) -> None:
        if self._heartbeat_task is not None and not self._heartbeat_task.done():
            return
        self._heartbeat_task = asyncio.create_task(self._heartbeat_loop())

    def _stop_heartbeat_if_idle(self) -> None:
        if self._running_sandbox_invocation_ids() or self._running_command_sessions():
            return
        if self._heartbeat_task is not None and not self._heartbeat_task.done():
            self._heartbeat_task.cancel()
        self._heartbeat_task = None

    async def _heartbeat_loop(self) -> None:
        while True:
            await asyncio.sleep(_HEARTBEAT_INTERVAL_S)
            by_sandbox = self._running_sandbox_invocation_ids()
            await self._collect_command_completions_once()
            if not by_sandbox and not self._running_command_sessions():
                self._heartbeat_task = None
                return
            for tracked in list(self._tasks.values()):
                if not _running_sandbox_task(tracked):
                    continue
                uptime_ms = max(
                    0.0, (time.monotonic() - tracked.started_at) * 1000.0
                )
                _emit_background_tool(
                    "background_tool.heartbeat",
                    tracked,
                    lane="sample",
                    uptime_ms=uptime_ms,
                )
            try:
                import sandbox.api as sandbox_api

                await asyncio.gather(
                    *(
                        sandbox_api.heartbeat(
                            sandbox_id,
                            invocation_ids,
                        )
                        for sandbox_id, invocation_ids in by_sandbox.items()
                    ),
                    return_exceptions=True,
                )
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.debug("background heartbeat iteration failed", exc_info=True)

    def _running_sandbox_invocation_ids(self) -> dict[str, list[str]]:
        by_sandbox: dict[str, list[str]] = {}
        for tracked in self._tasks.values():
            if (
                _running_sandbox_task(tracked)
                and tracked.sandbox_id
                and tracked.sandbox_invocation_id
                and tracked.heartbeat_enabled
            ):
                by_sandbox.setdefault(tracked.sandbox_id, []).append(
                    tracked.sandbox_invocation_id
                )
        return by_sandbox

    def _running_command_sessions(self) -> list[CommandSessionRecord]:
        return [
            record
            for record in self._command_sessions.values()
            if record.status == BackgroundTaskStatus.RUNNING
        ]

    async def _collect_command_completions_once(self) -> None:
        running = self._running_command_sessions()
        if not running:
            return
        try:
            import sandbox.api as sandbox_api

            by_sandbox_agent: dict[tuple[str, str], list[str]] = {}
            for record in running:
                by_sandbox_agent.setdefault(
                    (record.sandbox_id, record.agent_id),
                    [],
                ).append(record.command_session_id)
            for (sandbox_id, agent_id), ids in by_sandbox_agent.items():
                completions = await sandbox_api.collect_command_completions(
                    sandbox_id,
                    agent_id=agent_id,
                    command_session_ids=ids,
                )
                for completion in completions:
                    command_session_id = str(completion.get("command_session_id") or "")
                    record = self._command_sessions.get(command_session_id)
                    if record is None:
                        continue
                    result = completion.get("result")
                    status = (
                        str(result.get("status"))
                        if isinstance(result, dict)
                        else "completed"
                    )
                    record.result = dict(result) if isinstance(result, dict) else {}
                    if not record.command:
                        record.command = str(completion.get("command") or "")
                    if status == "cancelled":
                        record.status = BackgroundTaskStatus.CANCELLED
                    elif status == "ok":
                        record.status = BackgroundTaskStatus.COMPLETED
                    else:
                        record.status = BackgroundTaskStatus.FAILED
        except Exception:
            logger.debug("command-session completion collection failed", exc_info=True)


def _render_command_session_completion_notification(record: CommandSessionRecord) -> str:
    result = record.result or {}
    status = str(result.get("status") or record.status.value)
    exit_code = result.get("exit_code")
    output = result.get("output") if isinstance(result.get("output"), dict) else {}
    stdout = str(output.get("stdout") or "")
    stderr = str(output.get("stderr") or "")
    lines = [
        (
            f'[BACKGROUND COMPLETED] command_session_id="{record.command_session_id}" '
            f"status={status} exit_code={exit_code}"
        )
    ]
    if record.command:
        lines.append(f"command: {record.command}")
    if stdout:
        lines.append(f"stdout:\n{stdout.rstrip()}")
    if stderr:
        lines.append(f"stderr:\n{stderr.rstrip()}")
    return "\n".join(lines)


def _render_subagent_completion_notification(record: BackgroundTaskRecord) -> str:
    result = record.result
    metadata = result.metadata if result is not None else {}
    terminal_called = bool(metadata.get("subagent_terminal_called"))
    parent_exit_reason = (
        record.cancel_reason if record.stop_mode == _PARENT_EXIT_STOP_MODE else ""
    )
    if parent_exit_reason:
        status = "terminated"
    elif record.status == BackgroundTaskStatus.CANCELLED:
        status = "cancelled"
    elif record.status == BackgroundTaskStatus.COMPLETED and terminal_called:
        status = "finished"
    else:
        status = "failed"

    session_id = record.subagent_session_id or record.task_id
    agent_name = str(record.tool_input.get("agent_name") or "")
    lines = [
        (
            f'[SUBAGENT COMPLETED] subagent_session_id="{session_id}" '
            f"status={status}"
        )
    ]
    if agent_name:
        lines.append(f"agent_name: {agent_name}")
    if parent_exit_reason:
        lines.append(f"reason: {parent_exit_reason}")
    if result is not None and result.output:
        label = "result" if status == "finished" else "details"
        lines.append(f"{label}:\n{result.output.rstrip()}")
    if status == "failed":
        lines.append(
            "Use check_subagent_progress(subagent_session_id=..., last_n_messages=5) "
            "to inspect the final progress snapshot."
        )
    return "\n".join(lines)
