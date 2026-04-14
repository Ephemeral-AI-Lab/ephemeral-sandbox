"""Executor — pops ready Tasks and runs agents with deterministic result extraction."""

from __future__ import annotations

import asyncio
import inspect
import logging
import time
import uuid
from collections.abc import Awaitable
from typing import TYPE_CHECKING, Any, Callable

from team.models import AgentResult, BlockerDeclaration, Plan, ReplanPlan, ReplanRequest, RetryRequest, TaskStatus
from team.runtime.context_builder import TeamAgentContext

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
    """Pops ready tasks, runs agent, deterministic result extraction."""

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

    async def _checkpoint_after_transition(self, task: "Task", *, outcome: str) -> None:
        try:
            label = f"durable:{outcome}:{task.agent_name}:{task.id}"
            await self.team_run.checkpoint(label=label)
        except Exception:
            logger.debug("Failed to checkpoint after %s transition for %s", outcome, task.id, exc_info=True)

    async def run_forever(self) -> None:
        tc = self.team_run.task_center
        dq = self.team_run.dispatch_queue
        conductor = getattr(self.team_run, "conductor", None)
        pop_ready = dq.pop_ready
        pop_ready_accepts_guard = "blocker_guard" in inspect.signature(pop_ready).parameters
        while not self.team_run.cancel_event.is_set():
            try:
                if pop_ready_accepts_guard:
                    rec = await pop_ready(
                        self.team_run.id,
                        blocker_guard=getattr(conductor, "guard_pop_ready", None),
                    )
                else:
                    rec = await pop_ready(self.team_run.id)
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
            except Exception as exc:
                logger.exception("Worker error on %s: %s", task.id, exc)
                await tc.fail(task.id, f"worker_exception: {exc}")
                if conductor is not None:
                    blocker = conductor.blocker_for_fix_task(task.id)
                    if blocker is not None:
                        await conductor.on_fix_failed(blocker.id, f"worker_exception: {exc}")

    async def _run_one(self, task: "Task") -> None:
        tc = self.team_run.task_center
        conductor = getattr(self.team_run, "conductor", None)
        agent_run_id = str(uuid.uuid4())
        task = await tc.mark_running(task.id, agent_run_id)

        async def _current_task_state() -> Task | None:
            get_task = getattr(tc, "get_task", None)
            if callable(get_task):
                return await get_task(task.id)
            graph = getattr(tc, "graph", None)
            if isinstance(graph, dict):
                return graph.get(task.id, task)
            return task

        defn = self.agent_lookup(task.agent_name)
        if defn is None:
            await tc.fail(task.id, f"unknown_agent: {task.agent_name}")
            return

        await self._inject_scope_warnings(task)
        ctx = await self._build_context(defn, task)

        health_prefix = await self._plan_health_prefix(task)
        if health_prefix:
            ctx.user_message = health_prefix + "\n\n" + ctx.user_message

        listener = getattr(self.team_run, "scope_listener", None)
        if listener is not None and getattr(listener, "is_running", False) and task.scope_paths:
            from team.runtime.scope_change_buffer import ScopeChangeBuffer
            scope_buffer = ScopeChangeBuffer()
            listener.subscribe(agent_run_id, list(task.scope_paths), scope_buffer)
            if ctx.tool_metadata is not None:
                ctx.tool_metadata.extras["scope_change_buffer"] = scope_buffer

        runner_task: asyncio.Task[object] = asyncio.create_task(self.runner(defn, ctx))
        register_agent_run = getattr(self.team_run, "register_agent_run", None)
        if callable(register_agent_run):
            register_agent_run(task.id, runner_task)
        try:
            await runner_task
        except asyncio.CancelledError:
            current = await _current_task_state()
            if current is not None and current.status == TaskStatus.PAUSED:
                logger.info("Task %s paused during execution; dropping in-flight runner", task.id)
                return
            raise
        except Exception as exc:
            await tc.fail(task.id, f"runner_exception: {exc}")
            if conductor is not None:
                blocker = conductor.blocker_for_fix_task(task.id)
                if blocker is not None:
                    await conductor.on_fix_failed(blocker.id, f"runner_exception: {exc}")
            return
        finally:
            unregister_agent_run = getattr(self.team_run, "unregister_agent_run", None)
            if callable(unregister_agent_run):
                unregister_agent_run(task.id, runner_task)
            if listener is not None:
                listener.unsubscribe(agent_run_id)

        current = await _current_task_state()
        if current is not None and current.status == TaskStatus.PAUSED:
            logger.info("Task %s completed after pause; ignoring stale result", task.id)
            return

        result = await self._run_post_run(task, defn, ctx)
        await self._dispatch(task, result)

    async def _inject_scope_warnings(self, task: "Task") -> None:
        if not task.scope_paths:
            return
        store = getattr(self.team_run, "file_change_store", None)
        if store is None or not getattr(store, "initialized", False):
            return
        created_ts = task.created_at.timestamp() if task.created_at else 0.0
        changes = store.changes_since(created_ts)
        external = [
            e for e in changes
            if e.agent_run_id != (task.agent_run_id or "")
            and any(e.file_path.startswith(p.rstrip("/")) for p in task.scope_paths)
        ]
        if not external:
            return
        now = time.time()
        lines = ["## Warning: scope changes detected since plan creation",
                 "The following files in your scope were modified externally:"]
        for e in external:
            lines.append(f"- {e.file_path} ({e.edit_type} by {e.agent_id}, {int(now - e.created_at.timestamp())}s ago)")
        lines.append("Review these changes before proceeding. Call request_replan() if your task is no longer valid.")
        from team.models import Note
        try:
            await self.team_run.task_center.post(Note(
                id=str(uuid.uuid4()), task_id=task.id, agent_name="system",
                content="\n".join(lines), timestamp=now, scope_paths=list(task.scope_paths),
            ))
        except Exception:
            logger.debug("Failed to persist scope warning for %s", task.id, exc_info=True)

    async def _build_context(self, defn: "AgentDefinition", task: "Task") -> TeamAgentContext:
        if self.build_query_context is not None:
            return await self.build_query_context(defn, self.team_run, task)
        from team.runtime.context_builder import build_query_context
        return await build_query_context(defn, self.team_run, task)

    async def _plan_health_prefix(self, task: "Task") -> str | None:
        if not task.parent_id:
            return None
        try:
            stats = await self.team_run.task_center.sibling_stats(task.parent_id)
        except Exception:
            return None
        lines: list[str] = []
        done = stats.get("done", 0)
        failed = stats.get("failed", 0)
        started = done + failed
        retry_total = stats.get("retry_total", 0)
        if started >= 3 and started > 0 and failed / started > 0.4:
            lines.append(
                f"**PLAN HEALTH CRITICAL:** {failed}/{started} sibling tasks "
                f"have failed. Consider calling `request_replan()` if your "
                f"task depends on their output."
            )
        if retry_total >= 3:
            lines.append(
                f"**PLAN HEALTH WARNING:** {retry_total} retries across "
                f"sibling tasks. Check for systemic issues before proceeding."
            )
        return "\n".join(lines) if lines else None

    async def _run_post_run(
        self,
        task: "Task",
        defn: "AgentDefinition",
        ctx: TeamAgentContext,
    ) -> AgentResult | RetryRequest | ReplanRequest | BlockerDeclaration:
        """Post-run phase: extract submission from query-loop metadata.

        If the agent already submitted during the query loop (submit_plan,
        post_note, etc.), honour that directly. Only fall back to the
        streaming runner when no submission was captured in metadata.
        """
        legacy = self._posthook_legacy(ctx, defn)

        # If legacy extracted a real submission, use it directly
        if isinstance(legacy, (RetryRequest, ReplanRequest, BlockerDeclaration)):
            return legacy
        if isinstance(legacy, AgentResult) and (
            legacy.submitted_plan is not None
            or legacy.submitted_replan is not None
            or legacy.summary not in ("completed (no explicit submission)", "planner_did_not_submit_plan", "")
        ):
            return legacy

        # No submission in metadata — run post-run phase with posthook tools.
        # The agent is re-prompted with ONLY its posthook tools and must call
        # one of them. Loops up to 5 tool calls until a successful submission.
        from external_trigger.runner import run as run_trigger
        from tools.posthook.toolkit import PosthookTools

        api_client = getattr(self.team_run, "api_client", None)
        if api_client is None:
            return legacy

        conductor = getattr(self.team_run, "conductor", None)
        messages: list[dict] = []
        if conductor is not None:
            messages = conductor._executor_snapshots.get(task.id, [])

        # Get post_run tools by role
        toolkit = PosthookTools.from_context(ctx)
        post_run_tools = toolkit.list_tools()
        if not post_run_tools:
            return legacy
        tool_summary = ", ".join(f"{t.name}: {t.description}" for t in post_run_tools)

        try:
            result = await run_trigger(
                messages=messages,
                system_prompt="You are a task submission assistant.",
                prompt=(
                    "Your main work is complete. You must now submit your results "
                    f"by calling one of: {tool_summary}. "
                    "Summarize what you accomplished and call the appropriate tool."
                ),
                tools=post_run_tools,
                api_client=api_client,
                max_tokens_per_turn=1000,
                max_turns=5,
            )
        except (RuntimeError, Exception):
            logger.warning(
                "post_run streaming runner failed for task %s, falling back to legacy",
                task.id, exc_info=True,
            )
            return legacy

        # Map RunResult → domain objects
        tool_input = result.tool_input
        match result.tool_name:
            case "post_note":
                return AgentResult(summary=tool_input.get("content", ""))
            case "submit_plan":
                return AgentResult(summary="", submitted_plan=Plan.from_dict(tool_input))
            case "submit_replan" | "add_tasks" | "cancel_and_redraft":
                return AgentResult(summary="", submitted_replan=ReplanPlan.from_dict(tool_input))
            case "request_retry":
                return RetryRequest(reason=tool_input.get("reason", ""))
            case "request_replan":
                return ReplanRequest(
                    reason=tool_input.get("reason", ""),
                    suggestion=tool_input.get("suggestion"),
                )
            case "declare_blocker":
                return BlockerDeclaration(
                    root_cause_paths=tool_input.get("root_cause_paths", []),
                    reason=tool_input.get("reason", ""),
                    suggestion=tool_input.get("suggestion"),
                )
            case _:
                return AgentResult(summary=str(tool_input))

    @staticmethod
    def _posthook_legacy(ctx: TeamAgentContext, defn: "AgentDefinition") -> AgentResult | RetryRequest | ReplanRequest:
        """Legacy fallback: extract result from metadata when runner is unavailable."""
        metadata = ctx.tool_metadata
        submitted = metadata.get("submitted_output")
        if submitted is not None:
            if isinstance(submitted, Plan):
                return AgentResult(summary="", submitted_plan=submitted)
            if isinstance(submitted, ReplanPlan):
                return AgentResult(summary="", submitted_replan=submitted)
            if isinstance(submitted, RetryRequest):
                return submitted
            if isinstance(submitted, ReplanRequest):
                return submitted
            if isinstance(submitted, BlockerDeclaration):
                return submitted
            return AgentResult(summary=str(submitted))
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

    _extract_result = _posthook_legacy

    async def _post_completion_note(self, task: "Task", summary: str) -> None:
        if not summary or summary in ("completed (no explicit submission)", "planner_did_not_submit_plan"):
            return
        budget = getattr(self.team_run, "budgets", None)
        max_bytes = getattr(budget, "max_note_bytes", 100_000) if budget else 100_000
        from team.models import Note
        try:
            await self.team_run.task_center.post(Note(
                id=str(uuid.uuid4()), task_id=task.id,
                agent_name=task.agent_name or "unknown",
                content=summary[:max_bytes], timestamp=time.time(),
                scope_paths=list(task.scope_paths) if task.scope_paths else [],
            ))
        except Exception:
            logger.debug("completion note: post failed for %s", task.id, exc_info=True)

    async def _post_checkpoint_note(self, task: "Task", result: Any) -> str | None:
        tc = self.team_run.task_center
        try:
            stats = await tc.sibling_stats(task.parent_id)
        except Exception:
            logger.debug("checkpoint note: sibling_stats failed for %s", task.id, exc_info=True)
            return None
        lines = [f"**Checkpoint: {task.id} ({task.agent_name}) → {task.status}**"]
        if task.failure_reason:
            lines.append(f"Failure: {task.failure_reason}")
        fc_store = getattr(self.team_run, "file_change_store", None)
        if fc_store is not None and getattr(fc_store, "initialized", False) and task.agent_run_id:
            try:
                changes = fc_store.changes_by_agent_run(self.team_run.id, task.agent_run_id)
                if changes:
                    lines.append(f"Files touched: {', '.join(c.file_path for c in changes[:10])}")
            except Exception:
                pass
        action = None
        done, failed = stats.get("done", 0), stats.get("failed", 0)
        started = done + failed
        retry_total = stats.get("retry_total", 0)
        if started >= 3 and started > 0 and failed / started > 0.4:
            lines.append(f"PLAN HEALTH CRITICAL: {failed}/{started} sibling tasks failed")
            action = "replan"
        elif retry_total >= 3:
            lines.append(f"PLAN HEALTH WARNING: {retry_total} retries across sibling tasks")
        note_owner = task.parent_id or task.id
        from team.models import Note
        try:
            await tc.post(Note(
                id=str(uuid.uuid4()), task_id=note_owner, agent_name="checkpoint",
                content="\n".join(lines), timestamp=time.time(),
                scope_paths=list(task.scope_paths) if task.scope_paths else [],
            ))
        except Exception:
            logger.debug("checkpoint note: post failed for %s", task.id, exc_info=True)
        return action

    async def _post_retry_reason_note(self, task: "Task", result: RetryRequest) -> None:
        reason = getattr(result, "reason", "") or "no reason given"
        content = (
            f"**RETRY #{task.retry_count + 1}** — Previous attempt failed.\n"
            f"Reason: {reason}\n"
            f"Do NOT repeat the same approach. If this is your last retry "
            f"(max_retries={task.max_retries}), call `request_replan()` "
            f"so a replanner can restructure the work."
        )
        from team.models import Note
        try:
            await self.team_run.task_center.post(Note(
                id=str(uuid.uuid4()), task_id=task.id, agent_name="system",
                content=content, timestamp=time.time(),
                scope_paths=list(task.scope_paths) if task.scope_paths else [],
            ))
        except Exception:
            logger.debug("retry reason note: post failed for %s", task.id, exc_info=True)

    async def _dispatch(self, task: "Task", result: Any) -> None:
        tc = self.team_run.task_center
        conductor = getattr(self.team_run, "conductor", None)
        fix_blocker = conductor.blocker_for_fix_task(task.id) if conductor is not None else None
        if isinstance(result, RetryRequest):
            if fix_blocker is not None and conductor is not None:
                await tc.fail(task.id, f"blocker_fix_retry_requested: {result.reason}")
                await conductor.on_fix_failed(fix_blocker.id, result.reason)
                await self._checkpoint_after_transition(task, outcome="blocker_fix_failed")
                return
            await self._post_retry_reason_note(task, result)
            await tc.retry_task(task.id, result)
            await self._checkpoint_after_transition(task, outcome="retry")
            await self._post_checkpoint_note(task, result)
            return
        if isinstance(result, ReplanRequest):
            if fix_blocker is not None and conductor is not None:
                await tc.fail(task.id, f"blocker_fix_failed: {result.reason}")
                await conductor.on_fix_failed(fix_blocker.id, result.reason)
                await self._checkpoint_after_transition(task, outcome="blocker_fix_failed")
                return
            await tc.request_replan(task.id, result)
            await self._checkpoint_after_transition(task, outcome="replan_request")
            await self._post_checkpoint_note(task, result)
            return
        if isinstance(result, BlockerDeclaration):
            conductor = getattr(self.team_run, "conductor", None)
            if conductor is not None:
                await conductor.create_blocker(
                    reason=result.reason,
                    root_cause_paths=result.root_cause_paths,
                    initiating_task_id=task.id,
                    declared_by=task.id,
                )
            await tc.complete_task(task.id, AgentResult(summary=f"Declared blocker: {result.reason}"))
            await self._checkpoint_after_transition(task, outcome="blocker_declared")
            return
        new_items = await tc.complete_task(task.id, result)
        if isinstance(result, AgentResult) and result.summary:
            await self._post_completion_note(task, result.summary)
        if fix_blocker is not None and conductor is not None:
            await conductor.on_fix_complete(
                fix_blocker.id,
                result.summary if isinstance(result, AgentResult) and result.summary else f"Fix task {task.id} completed.",
            )
        if self.after_dispatch is not None:
            cb = self.after_dispatch(task, result, new_items)
            if isinstance(cb, Awaitable):
                await cb
        await self._checkpoint_after_transition(task, outcome="complete")
        await self._post_checkpoint_note(task, result)
