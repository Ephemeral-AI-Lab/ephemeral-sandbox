"""Executor — runs one task's agent and returns a single ``TaskStatusUpdate``.

Every outcome (success, plan, replan, request_replan, runner exception,
cooperative cancellation, unknown-agent, no-terminal-call) is encoded in the
returned update. ``TaskQueue`` hands the update to ``TaskStatusHandler``,
which owns every graph-level transition.

The executor is the only writer of ``ready → running`` (via
``store.mark_running``); every other status change is owned by the handler.
"""

from __future__ import annotations

import asyncio
import logging
import uuid
from collections.abc import Awaitable, Coroutine
from typing import TYPE_CHECKING, Any, Callable

from team.models import (
    Plan,
    ReplanPlan,
    TaskStatus,
    TaskStatusUpdate,
)
from team.persistence.events import make_task_status
from team.runtime.context_builder import TeamAgentContext
from team.runtime.scope_change_notifier import ScopeChangeNotifier

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.models import Task
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)

QueryRunner = Callable[["AgentDefinition", Any], Coroutine[Any, Any, Any]]
QueryContextBuilder = Callable[["AgentDefinition", "TeamRun", "Task"], Awaitable[TeamAgentContext]]
AfterDispatch = Callable[["Task", TaskStatusUpdate], Any]


def translate_tool_metadata(task_id: str, ctx: TeamAgentContext) -> TaskStatusUpdate:
    """Translate a runner's ``ctx.tool_metadata`` into a single status update.

    Four outcomes, matching the agent-producible kinds in the design:

    - ``task_summary_type == "success"``         → ``SUCCESS`` with summary
    - ``task_summary_type == "request_replan"``  → ``REQUEST_REPLAN`` with reason
    - ``resolved_plan`` set (Plan / ReplanPlan)  → ``EXPANDED`` with the payload
    - everything else                            → ``FAILED`` ("no terminal call")

    This is the only reader of ``tool_metadata``.
    """
    meta = ctx.tool_metadata
    kind = meta.get("task_summary_type")
    summary = str(meta.get("task_summary") or "")

    if kind == "success":
        return TaskStatusUpdate(task_id=task_id, status=TaskStatus.DONE, summary=summary)

    if kind == "request_replan":
        return TaskStatusUpdate(
            task_id=task_id, status=TaskStatus.REQUEST_REPLAN, summary=summary
        )

    resolved = meta.get("resolved_plan")
    if resolved is not None:
        if meta.get("plan_is_replan") and isinstance(resolved, ReplanPlan):
            return TaskStatusUpdate(
                task_id=task_id, status=TaskStatus.EXPANDED, replan=resolved
            )
        if isinstance(resolved, Plan):
            return TaskStatusUpdate(
                task_id=task_id, status=TaskStatus.EXPANDED, plan=resolved
            )

    tail = str(meta.get("work_result") or "")[:500]
    reason = "no_terminal_tool_called"
    if tail:
        reason = f"{reason}: {tail}"
    return TaskStatusUpdate(task_id=task_id, status=TaskStatus.FAILED, summary=reason)


class Executor:
    """Runs one task's agent run and returns a ``TaskStatusUpdate``."""

    def __init__(
        self,
        team_run: "TeamRun",
        runner: QueryRunner,
        agent_lookup: Callable[[str], "AgentDefinition | None"],
        build_query_context: QueryContextBuilder | None = None,
        after_dispatch: AfterDispatch | None = None,
    ) -> None:
        self.team_run = team_run
        self.runner = runner
        self.agent_lookup = agent_lookup
        self.build_query_context = build_query_context
        self.after_dispatch = after_dispatch
        self.scope_notifier = ScopeChangeNotifier(team_run)

    async def run(self, task_id: str) -> TaskStatusUpdate:
        """Claim and run one task; return the outcome update (no handler call)."""
        tc = self.team_run.task_center
        agent_run_id = str(uuid.uuid4())
        rec = await tc.store.mark_running(task_id, agent_run_id)
        if rec is None:
            return TaskStatusUpdate(
                task_id=task_id,
                status=TaskStatus.FAILED,
                summary="mark_running_failed: task not in ready/running state",
            )
        task = tc.store.get_task(task_id)
        if task is None:
            return TaskStatusUpdate(
                task_id=task_id,
                status=TaskStatus.FAILED,
                summary="mark_running_failed: task not in graph after claim",
            )
        tc.emit_event(
            make_task_status(
                self.team_run.id,
                task_id,
                "running",
                agent_run_id=agent_run_id,
                started_at=task.started_at.isoformat() if task.started_at else None,
            )
        )

        defn = self.agent_lookup(task.agent_name)
        if defn is None:
            return TaskStatusUpdate(
                task_id=task_id,
                status=TaskStatus.FAILED,
                summary=f"unknown_agent: {task.agent_name}",
            )

        await self.scope_notifier.inject_warning(task)
        ctx = await self._build_context(defn, task)

        runner_task: asyncio.Task[object] = asyncio.create_task(self.runner(defn, ctx))
        self.team_run.register_agent_run(task_id, runner_task)
        try:
            try:
                await runner_task
            except asyncio.CancelledError:
                if self.team_run.cancel_event.is_set():
                    return TaskStatusUpdate(
                        task_id=task_id,
                        status=TaskStatus.CANCELLED,
                        summary="cooperative_cancel",
                    )
                raise
            except Exception as exc:
                return TaskStatusUpdate(
                    task_id=task_id,
                    status=TaskStatus.FAILED,
                    summary=f"runner_exception: {exc}",
                )
            return translate_tool_metadata(task_id, ctx)
        finally:
            self.team_run.unregister_agent_run(task_id, runner_task)

    async def post_dispatch(self, update: TaskStatusUpdate) -> None:
        """Fire the optional ``after_dispatch`` hook after the handler returns."""
        if self.after_dispatch is None:
            return
        task = self.team_run.task_center.store.get_task(update.task_id)
        if task is None:
            return
        cb = self.after_dispatch(task, update)
        if isinstance(cb, Awaitable):
            await cb

    async def _build_context(self, defn: "AgentDefinition", task: "Task") -> TeamAgentContext:
        if self.build_query_context is not None:
            return await self.build_query_context(defn, self.team_run, task)
        from team.runtime.context_builder import build_query_context

        return await build_query_context(defn, self.team_run, task)
