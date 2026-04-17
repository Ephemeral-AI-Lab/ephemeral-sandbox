"""Executor — pops ready Tasks, runs agents, dispatches on task state.

Tools write structured data to ``ctx.tool_metadata`` during the main run.
The executor reads that state after the runner returns and dispatches:
complete, submit_plan, submit_replan, or submit_task_summary.
"""

from __future__ import annotations

import asyncio
import logging
import time
import uuid
from collections.abc import Awaitable
from typing import TYPE_CHECKING, Any, Callable

from team.errors import BudgetExceeded, GraphInvariantViolation
from team.models import AgentResult, Plan, ReplanPlan, ReplanRequest
from team.runtime.context_builder import TeamAgentContext
from team.runtime.plan_health_monitor import PlanHealthMonitor
from team.runtime.scope_change_notifier import ScopeChangeNotifier

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.models import Task
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)

QueryRunner = Callable[["AgentDefinition", Any], Awaitable[Any]]
QueryContextBuilder = Callable[["AgentDefinition", "TeamRun", "Task"], Awaitable[TeamAgentContext]]


def _record_to_task(rec: Any) -> "Task":
    from team.persistence.task_store import record_to_task

    return record_to_task(rec)


class Executor:
    """Pops ready tasks, runs agent, reads task state, dispatches."""

    def __init__(
        self,
        team_run: "TeamRun",
        runner: QueryRunner,
        agent_lookup: Callable[[str], "AgentDefinition | None"],
        build_query_context: QueryContextBuilder | None = None,
        after_dispatch: Callable[["Task", AgentResult, list["Task"]], Any] | None = None,
    ) -> None:
        self.team_run = team_run
        self.runner = runner
        self.agent_lookup = agent_lookup
        self.build_query_context = build_query_context
        self.after_dispatch = after_dispatch
        self.plan_health = PlanHealthMonitor(team_run)
        self.scope_notifier = ScopeChangeNotifier(team_run)

    async def _checkpoint_after_transition(self, task: "Task", *, outcome: str) -> None:
        try:
            label = f"durable:{outcome}:{task.agent_name}:{task.id}"
            await self.team_run.checkpoint(label=label)
        except Exception:
            logger.debug(
                "Failed to checkpoint after %s transition for %s", outcome, task.id, exc_info=True
            )

    async def _handle_worker_exception(self, task: "Task", reason: str) -> None:
        await self.team_run.task_center.fail_task(task.id, reason)

    _DEADLOCK_IDLE_THRESHOLD = 100  # ~5s of idle polls (100 * 50ms)

    async def _check_deadlock(self) -> bool:
        """Return True if all remaining tasks are stuck (no ready, no running)."""
        if getattr(self.team_run, "_dispatching", 0) > 0:
            return False
        active = getattr(self.team_run, "_active_agent_runs", {})
        if active:
            return False
        try:
            statuses = await self.team_run.task_center._store.get_statuses()
        except Exception:
            return False
        has_pending = any(s in ("pending", "ready") for s in statuses.values())
        has_running = any(s == "running" for s in statuses.values())
        has_replanning = any(s == "replanning" for s in statuses.values())
        if has_replanning and not has_running and not has_pending:
            await self.team_run.task_center.fail_orphaned_replanning()
            return False
        return has_pending and not has_running

    async def run_forever(self) -> None:
        tc = self.team_run.task_center
        dq = self.team_run.dispatch_queue
        pop_ready = dq.pop_ready
        idle_polls = 0
        while not self.team_run.cancel_event.is_set():
            try:
                rec = await pop_ready(self.team_run.id)
            except GraphInvariantViolation as exc:
                await self._fail_team_run_for_invariant(exc)
                break
            except Exception as exc:
                logger.exception("DispatchQueue pop_ready failed: %s", exc)
                await asyncio.sleep(0.2)
                continue
            if rec is None:
                idle_polls += 1
                if idle_polls >= self._DEADLOCK_IDLE_THRESHOLD and await self._check_deadlock():
                    logger.error(
                        "Deadlock detected: pending tasks remain but none are ready or running"
                    )
                    self.team_run.cancel_event.set()
                    break
                await asyncio.sleep(0.05)
                continue
            idle_polls = 0
            task = _record_to_task(rec)
            tc.graph[task.id] = task
            try:
                await self._run_one(task)
            except GraphInvariantViolation as exc:
                await self._fail_team_run_for_invariant(exc)
                break
            except Exception as exc:
                logger.exception("Worker error on %s: %s", task.id, exc)
                try:
                    await self._handle_worker_exception(task, f"worker_exception: {exc}")
                except GraphInvariantViolation as invariant:
                    await self._fail_team_run_for_invariant(invariant)
                    break

    async def _fail_team_run_for_invariant(self, exc: GraphInvariantViolation) -> None:
        reason = f"graph_invariant_violation: {exc}"
        logger.critical(reason)
        fail_fast = getattr(self.team_run, "fail_fast", None)
        if callable(fail_fast):
            await fail_fast(reason)
        else:
            self.team_run.cancel_event.set()

    async def _run_one(self, task: "Task") -> None:
        self.team_run._dispatching = getattr(self.team_run, "_dispatching", 0) + 1
        try:
            await self._run_one_inner(task)
        finally:
            self.team_run._dispatching = max(0, getattr(self.team_run, "_dispatching", 1) - 1)

    async def _run_one_inner(self, task: "Task") -> None:
        tc = self.team_run.task_center
        agent_run_id = str(uuid.uuid4())
        task = await tc.mark_running(task.id, agent_run_id)

        defn = self.agent_lookup(task.agent_name)
        if defn is None:
            await tc.fail_task(task.id, f"unknown_agent: {task.agent_name}")
            return

        await self._inject_scope_warnings(task)
        ctx = await self._build_context(defn, task)

        health_prefix = await self._plan_health_prefix(task)
        if health_prefix:
            ctx.user_message = health_prefix + "\n\n" + ctx.user_message

        runner_task: asyncio.Task[object] = asyncio.create_task(self.runner(defn, ctx))
        register_agent_run = getattr(self.team_run, "register_agent_run", None)
        if callable(register_agent_run):
            register_agent_run(task.id, runner_task)
        try:
            await runner_task
        except asyncio.CancelledError:
            raise
        except GraphInvariantViolation:
            raise
        except Exception as exc:
            await self._handle_worker_exception(task, f"runner_exception: {exc}")
            return
        finally:
            unregister_agent_run = getattr(self.team_run, "unregister_agent_run", None)
            if callable(unregister_agent_run):
                unregister_agent_run(task.id, runner_task)

        # --- Read task state from tool_metadata and dispatch ---
        result = self._read_result(task, ctx)
        await self._dispatch(task, result)

    def _read_result(
        self,
        task: "Task",
        ctx: TeamAgentContext,
    ) -> AgentResult | ReplanRequest:
        """Read structured result from tool_metadata written by submission tools."""
        meta = ctx.tool_metadata
        summary_type = meta.get("task_summary_type")
        summary = str(meta.get("task_summary") or "")

        if summary_type == "success":
            return AgentResult(summary=summary)

        if summary_type == "fail":
            return ReplanRequest(reason=summary)

        # submit_plan or submit_replan was called.
        resolved_plan = meta.get("resolved_plan")
        if resolved_plan is not None:
            is_replan = bool(meta.get("plan_is_replan"))
            if is_replan and isinstance(resolved_plan, ReplanPlan):
                return AgentResult(summary="", submitted_replan=resolved_plan)
            elif isinstance(resolved_plan, Plan):
                return AgentResult(summary="", submitted_plan=resolved_plan)

        # Fallback: no terminal tool was called (runner exhausted retries)
        work_result = str(meta.get("work_result") or "")[:500]
        return AgentResult(summary=f"completed (no submission): {work_result}".strip())

    async def _inject_scope_warnings(self, task: "Task") -> None:
        await self.scope_notifier.inject_warning(task)

    async def _build_context(self, defn: "AgentDefinition", task: "Task") -> TeamAgentContext:
        if self.build_query_context is not None:
            return await self.build_query_context(defn, self.team_run, task)
        from team.runtime.context_builder import build_query_context

        return await build_query_context(defn, self.team_run, task)

    async def _plan_health_prefix(self, task: "Task") -> str | None:
        return await self.plan_health.compute_prefix(task)

    async def _post_completion_note(self, task: "Task", summary: str) -> None:
        if not summary or summary.startswith("completed ("):
            return
        budget = getattr(self.team_run, "budgets", None)
        max_bytes = getattr(budget, "max_note_bytes", 100_000) if budget else 100_000
        from team.models import Note

        try:
            await self.team_run.task_center.notes.post(
                Note(
                    id=str(uuid.uuid4()),
                    task_id=task.id,
                    agent_name=task.agent_name or "unknown",
                    content=summary[:max_bytes],
                    timestamp=time.time(),
                    paths=list(task.scope_paths) if task.scope_paths else [],
                    tags=["implementation"],
                )
            )
        except Exception:
            logger.debug("completion note: post failed for %s", task.id, exc_info=True)

    async def _post_checkpoint_note(self, task: "Task", result: Any) -> str | None:
        return await self.plan_health.post_checkpoint_note(task, result)

    async def _dispatch(self, task: "Task", result: Any) -> None:
        tc = self.team_run.task_center

        if isinstance(result, ReplanRequest):
            try:
                await tc.request_replan(task.id, result)
            except BudgetExceeded as exc:
                # Replan budget exhausted — gracefully complete the task
                # instead of failing+retrying in an infinite loop.
                logger.warning(
                    "request_replan for task %s failed (%s); completing task as-is",
                    task.id,
                    exc,
                )
                await tc.complete_task(
                    task.id,
                    AgentResult(summary=f"replan_budget_exhausted: {result.reason}"),
                )
                await self._checkpoint_after_transition(task, outcome="replan_budget_exhausted")
                return
            await self._checkpoint_after_transition(task, outcome="replan_request")
            await self._post_checkpoint_note(task, result)
            return

        new_items = await tc.complete_task(task.id, result)
        if isinstance(result, AgentResult) and result.summary:
            await self._post_completion_note(task, result.summary)
        if self.after_dispatch is not None:
            cb = self.after_dispatch(task, result, new_items)
            if isinstance(cb, Awaitable):
                await cb
        await self._checkpoint_after_transition(task, outcome="complete")
        await self._post_checkpoint_note(task, result)
