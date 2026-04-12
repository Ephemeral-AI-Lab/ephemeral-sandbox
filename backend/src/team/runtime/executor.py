"""Executor — pops ready Tasks and runs agents with deterministic result extraction."""

from __future__ import annotations

import asyncio
import logging
import time
import uuid
from collections.abc import Awaitable
from typing import TYPE_CHECKING, Any, Callable

from team.models import AgentResult, Plan, ReplanPlan, ReplanRequest, RetryRequest, SubmittedSummary
from team.runtime.context_builder import TeamAgentContext

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.models import Task
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)

QueryRunner = Callable[["AgentDefinition", Any], Awaitable[Any]]


class Executor:
    """Pops ready tasks, runs agent, deterministic result extraction."""

    def __init__(
        self,
        team_run: "TeamRun",
        runner: QueryRunner,
        agent_lookup: Callable[[str], "AgentDefinition | None"],
        after_dispatch: Callable[["Task", AgentResult, list["Task"]], Any] | None = None,
    ) -> None:
        self.team_run = team_run
        self.runner = runner
        self.agent_lookup = agent_lookup
        self.after_dispatch = after_dispatch

    async def _checkpoint_after_transition(self, task: "Task", *, outcome: str) -> None:
        """Persist a post-dispatch checkpoint after the dispatcher state mutates."""
        try:
            label = f"durable:{outcome}:{task.agent_name}:{task.id}"
            await self.team_run.checkpoint(label=label)
        except Exception:
            logger.debug("Failed to checkpoint after %s transition for %s", outcome, task.id, exc_info=True)

    async def run_forever(self) -> None:
        """Pop READY tasks until cancel_event is set."""
        dispatcher = self.team_run.dispatcher
        while not self.team_run.cancel_event.is_set():
            try:
                task_id = await asyncio.wait_for(dispatcher.pop_ready(), timeout=0.1)
            except asyncio.TimeoutError:
                continue
            try:
                await self._run_one(task_id)
            except Exception as exc:
                logger.exception("Worker error on %s: %s", task_id, exc)
                await dispatcher.fail(task_id, f"worker_exception: {exc}")

    async def _run_one(self, task_id: str) -> None:
        dispatcher = self.team_run.dispatcher
        agent_run_id = str(uuid.uuid4())
        task = await dispatcher.mark_running(task_id, agent_run_id)

        defn = self.agent_lookup(task.agent_name)
        if defn is None:
            await dispatcher.fail(task_id, f"unknown_agent: {task.agent_name}")
            return

        # Pre-start: check if files in scope changed externally since task creation
        self._inject_scope_warnings(task)

        ctx = self._build_context(defn, task)
        try:
            await self.runner(defn, ctx)
        except Exception as exc:
            await dispatcher.fail(task_id, f"runner_exception: {exc}")
            return

        result = self._posthook(ctx, defn)
        await self._dispatch(task_id, task, result)

    def _inject_scope_warnings(self, task: "Task") -> None:
        """Check if files in task's scope changed since plan creation.

        If external changes are detected, inject a warning note into the
        Task Center so the agent sees it in context_for(). The agent
        decides whether to proceed or request_replan()."""
        if not task.scope_paths:
            return
        ledger = getattr(self.team_run, "ledger", None)
        if ledger is None:
            return
        created_ts = task.created_at.timestamp() if task.created_at else 0.0
        changes = ledger.changes_since(created_ts)
        # Filter to scope and exclude changes by this task's own agent run
        external = [
            e for e in changes
            if e.agent_id != (task.agent_run_id or "")
            and any(e.file_path.startswith(p.rstrip("/")) for p in task.scope_paths)
        ]
        if not external:
            return
        now = time.time()
        lines = ["## Warning: scope changes detected since plan creation",
                 "The following files in your scope were modified externally:"]
        for e in external:
            lines.append(f"- {e.file_path} ({e.edit_type} by {e.agent_id}, "
                         f"{int(now - e.timestamp)}s ago)")
        lines.append("Review these changes before proceeding. "
                      "Call request_replan() if your task is no longer valid.")
        from team.models import Note
        self.team_run.task_center.post(Note(
            id=str(uuid.uuid4()),
            task_id=task.id,
            agent_name="system",
            content="\n".join(lines),
            timestamp=now,
            scope_paths=list(task.scope_paths),
        ))

    def _build_context(self, defn: "AgentDefinition", task: "Task") -> TeamAgentContext:
        """Build agent context using the canonical build_query_context."""
        from team.runtime.context_builder import build_query_context
        return build_query_context(defn, self.team_run, task)

    @staticmethod
    def _posthook(ctx: TeamAgentContext, defn: "AgentDefinition") -> AgentResult | RetryRequest | ReplanRequest:
        """Deterministic result extraction — no LLM call, always produces a result."""
        metadata = ctx.tool_metadata
        submitted = metadata.get("submitted_output")

        if submitted is not None:
            if isinstance(submitted, Plan):
                return AgentResult(summary="", submitted_plan=submitted)
            if isinstance(submitted, ReplanPlan):
                return AgentResult(summary="", submitted_replan=submitted)
            if isinstance(submitted, SubmittedSummary):
                return AgentResult(summary=submitted.summary)
            if isinstance(submitted, RetryRequest):
                return submitted
            if isinstance(submitted, ReplanRequest):
                return submitted
            return AgentResult(summary=str(submitted))

        # No submission — role-aware fallback (use registry for consistency with Dispatcher.complete)
        from agents.registry import has_role
        role = getattr(defn, "role", "")
        if not role and hasattr(defn, "name"):
            role = "planner" if has_role(defn.name, "planner") else ""
        if role == "planner":
            return AgentResult(summary="planner_did_not_submit_plan")

        work_result = metadata.get("work_result")
        if isinstance(work_result, str) and work_result.strip():
            return AgentResult(summary=work_result[:2000])
        return AgentResult(summary="completed (no explicit submission)")

    async def _dispatch(self, task_id: str, task: "Task", result: Any) -> None:
        dispatcher = self.team_run.dispatcher
        if isinstance(result, RetryRequest):
            await dispatcher.retry_work_item(task_id, result)
            await self._checkpoint_after_transition(task, outcome="retry")
            return
        if isinstance(result, ReplanRequest):
            await dispatcher.request_replan(task_id, result)
            await self._checkpoint_after_transition(task, outcome="replan_request")
            return
        new_items = await dispatcher.complete(task_id, result)
        if self.after_dispatch is not None:
            cb = self.after_dispatch(task, result, new_items)
            if isinstance(cb, Awaitable):
                await cb
        await self._checkpoint_after_transition(task, outcome="complete")
