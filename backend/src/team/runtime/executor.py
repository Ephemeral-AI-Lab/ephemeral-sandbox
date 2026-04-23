"""Executor — pops ready Tasks, runs agents, dispatches on task state.

Tools write structured data to ``ctx.tool_metadata`` during the main run.
The executor reads that state after the runner returns and dispatches:
complete, submit_plan, submit_replan, submit_task_success, or request_replan.
"""

from __future__ import annotations

import asyncio
import logging
import time
import uuid
from collections.abc import Awaitable, Coroutine
from typing import TYPE_CHECKING, Any, Callable

from sqlalchemy.exc import SQLAlchemyError

from team.errors import BudgetExceeded, GraphInvariantViolation
from team.models import AgentResult, Plan, ReplanPlan, ReplanRequest
from team.runtime.context_builder import TeamAgentContext
from team.runtime.scope_change_notifier import ScopeChangeNotifier

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.models import Task
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)

QueryRunner = Callable[["AgentDefinition", Any], Coroutine[Any, Any, Any]]
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
        self.scope_notifier = ScopeChangeNotifier(team_run)

    async def _task_status_value(self, task_id: str) -> str | None:
        tc = self.team_run.task_center
        task = None
        get_task = getattr(tc, "get_task", None)
        if get_task is not None:
            try:
                maybe_task = get_task(task_id)
                task = await maybe_task if isinstance(maybe_task, Awaitable) else maybe_task
            except Exception:
                logger.debug("failed to read task status for %s", task_id, exc_info=True)
        if task is None:
            graph = getattr(tc, "graph", None)
            if isinstance(graph, dict):
                task = graph.get(task_id)
        status = getattr(task, "status", None)
        return getattr(status, "value", status)

    async def _checkpoint_after_transition(self, task: "Task", *, outcome: str) -> None:
        try:
            label = f"durable:{outcome}:{task.agent_name}:{task.id}"
            await self.team_run.checkpoint(label=label)
        except Exception:
            logger.debug(
                "Failed to checkpoint after %s transition for %s", outcome, task.id, exc_info=True
            )

    async def _handle_worker_exception(
        self,
        task: "Task",
        reason: str,
        *,
        fatal: bool = False,
    ) -> None:
        tc = self.team_run.task_center
        if fatal and hasattr(tc, "force_fail_task"):
            await tc.force_fail_task(task.id, reason)
        else:
            await tc.fail_task(task.id, reason)
        if fatal:
            await self.team_run.fail_fast(reason)

    async def run_forever(self) -> None:
        tc = self.team_run.task_center
        dq = self.team_run.dispatch_queue
        pop_ready = dq.pop_ready
        # Restart-recovery: any parent left in EXPANDED_AWAITING_SUMMARY with
        # no live parent-summary sidecar gets one re-injected. Summary tasks
        # already READY in the DB are picked up by the normal dispatch loop.
        try:
            store = getattr(tc, "store", None)
            if store is not None and hasattr(store, "fetch_parents_awaiting_summary"):
                stuck_parents = await store.fetch_parents_awaiting_summary()
                if stuck_parents:
                    results = await asyncio.gather(
                        *(
                            tc._ensure_parent_summary_task(pid)
                            for pid in stuck_parents
                        ),
                        return_exceptions=True,
                    )
                    for pid, outcome in zip(stuck_parents, results):
                        if isinstance(outcome, Exception):
                            logger.exception(
                                "Failed to re-inject parent-summary task for %s",
                                pid,
                                exc_info=outcome,
                            )
        except Exception:
            logger.exception("Restart recovery for awaiting-summary parents failed")
        while not self.team_run.cancel_event.is_set():
            try:
                rec = await pop_ready(self.team_run.id)
            except GraphInvariantViolation as exc:
                await self.team_run.fail_fast(f"graph_invariant_violation: {exc}")
                break
            except Exception as exc:
                logger.exception("DispatchQueue pop_ready failed: %s", exc)
                await asyncio.sleep(0.2)
                continue
            if rec is None:
                await asyncio.sleep(0.05)
                continue
            task = _record_to_task(rec)
            tc.graph[task.id] = task
            try:
                await self._run_one(task)
            except GraphInvariantViolation as exc:
                await self.team_run.fail_fast(f"graph_invariant_violation: {exc}")
                break
            except BudgetExceeded as exc:
                await self.team_run.fail_fast(f"tasks_budget_exhausted: {exc}")
                break
            except Exception as exc:
                logger.exception("Worker error on %s: %s", task.id, exc)
                fatal = isinstance(exc, SQLAlchemyError)
                try:
                    await self._handle_worker_exception(
                        task,
                        f"worker_exception: {exc}",
                        fatal=fatal,
                    )
                except GraphInvariantViolation as invariant:
                    await self.team_run.fail_fast(f"graph_invariant_violation: {invariant}")
                    break
                except Exception as cleanup_exc:
                    logger.exception(
                        "Worker error cleanup failed for %s: %s",
                        task.id,
                        cleanup_exc,
                    )
                    await self.team_run.fail_fast(
                        f"worker_exception_cleanup_failed: {cleanup_exc}"
                    )
                    break
                if fatal:
                    break

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

        runner_task: asyncio.Task[object] = asyncio.create_task(self.runner(defn, ctx))
        self.team_run.register_agent_run(task.id, runner_task)
        try:
            await runner_task
        except asyncio.CancelledError:
            if not self.team_run.cancel_event.is_set():
                status = await self._task_status_value(task.id)
                if status == "cancelled":
                    logger.info("Worker task %s was cancelled by graph transition", task.id)
                    return
            raise
        except GraphInvariantViolation:
            raise
        except Exception as exc:
            await self._handle_worker_exception(task, f"runner_exception: {exc}")
            return
        finally:
            self.team_run.unregister_agent_run(task.id, runner_task)

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

        if summary_type == "request_replan":
            return ReplanRequest(reason=summary, explicit=True)

        # submit_plan or submit_replan was called.
        resolved_plan = meta.get("resolved_plan")
        if resolved_plan is not None:
            is_replan = bool(meta.get("plan_is_replan"))
            if is_replan and isinstance(resolved_plan, ReplanPlan):
                return AgentResult(summary="", submitted_replan=resolved_plan)
            elif isinstance(resolved_plan, Plan):
                return AgentResult(summary="", submitted_plan=resolved_plan)

        # Fallback: no terminal tool was called.
        work_result = str(meta.get("work_result") or "")[:500]
        reason = "Agent did not call a terminal submission tool."
        if work_result:
            reason = f"{reason} Last output: {work_result}"
        return ReplanRequest(reason=reason)

    async def _inject_scope_warnings(self, task: "Task") -> None:
        await self.scope_notifier.inject_warning(task)

    async def _build_context(self, defn: "AgentDefinition", task: "Task") -> TeamAgentContext:
        if self.build_query_context is not None:
            return await self.build_query_context(defn, self.team_run, task)
        from team.runtime.context_builder import build_query_context

        return await build_query_context(defn, self.team_run, task)

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

    async def _dispatch(self, task: "Task", result: Any) -> None:
        tc = self.team_run.task_center

        if isinstance(result, ReplanRequest):
            # Parent-summary sidecars must not be rescued by a replanner.
            # A no-terminal-call outcome after retries is a hard failure. An
            # explicit request_replan from the summarizer is different: the
            # roll-up found unresolved parent work, so replan the summarized
            # parent instead of incorrectly finalizing it as DONE.
            from agents.registry import get_role

            if get_role(task.agent_name) == "parent_summarizer":
                if result.explicit and task.fired_by_task_id:
                    try:
                        await tc.request_replan(task.fired_by_task_id, result)
                    except BudgetExceeded as exc:
                        reason = f"replan_budget_exhausted: {exc}"
                        await tc.fail_task(task.id, reason)
                        await self.team_run.fail_fast(reason)
                        await self._checkpoint_after_transition(
                            task,
                            outcome="replan_budget_exhausted",
                        )
                        return
                    await tc.complete_task(task.id, AgentResult(summary=result.reason))
                    await self._checkpoint_after_transition(
                        task, outcome="parent_summary_request_replan"
                    )
                    return
                await tc.fail_task(
                    task.id, "parent_summary_no_terminal_call"
                )
                await self._checkpoint_after_transition(
                    task, outcome="parent_summary_no_terminal_call"
                )
                return
            try:
                await tc.request_replan(task.id, result)
            except BudgetExceeded as exc:
                reason = f"replan_budget_exhausted: {exc}"
                await tc.fail_task(task.id, reason)
                await self.team_run.fail_fast(reason)
                await self._checkpoint_after_transition(
                    task,
                    outcome="replan_budget_exhausted",
                )
                return
            await self._checkpoint_after_transition(task, outcome="replan_request")
            return

        new_items = await tc.complete_task(task.id, result)
        if isinstance(result, AgentResult) and result.summary:
            await self._post_completion_note(task, result.summary)
        if self.after_dispatch is not None:
            cb = self.after_dispatch(task, result, new_items)
            if isinstance(cb, Awaitable):
                await cb
        await self._checkpoint_after_transition(task, outcome="complete")
