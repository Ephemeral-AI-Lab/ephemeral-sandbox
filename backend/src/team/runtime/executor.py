"""Executor — pops ready Tasks and runs agents with deterministic result extraction."""

from __future__ import annotations

import asyncio
import logging
import sys
import time
import uuid
from collections.abc import Awaitable
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable

from team.models import AgentResult, BlockerDeclaration, Plan, ReplanPlan, ReplanRequest, TaskStatus
from team.runtime.context_builder import TeamAgentContext
from team.runtime.plan_health_monitor import PlanHealthMonitor
from team.runtime.scope_change_notifier import ScopeChangeNotifier
from tools.core.base import ToolExecutionContext

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


def _extract_json_payload(text: str) -> dict[str, Any] | None:
    """Extract the first dict-shaped JSON object from free text, preferring fences."""
    import json, re

    for block in re.findall(r"```(?:json)?\s*(\{[\s\S]*?\})\s*```", text):
        try:
            obj = json.loads(block)
            if isinstance(obj, dict):
                return obj
        except Exception:
            continue
    start = text.find("{")
    while start != -1:
        depth = 0
        for i in range(start, len(text)):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    try:
                        obj = json.loads(text[start : i + 1])
                        if isinstance(obj, dict):
                            return obj
                    except Exception:
                        pass
                    break
        start = text.find("{", start + 1)
    return None


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
        self.plan_health = PlanHealthMonitor(team_run)
        self.scope_notifier = ScopeChangeNotifier(team_run)

    async def _checkpoint_after_transition(self, task: "Task", *, outcome: str) -> None:
        try:
            label = f"durable:{outcome}:{task.agent_name}:{task.id}"
            await self.team_run.checkpoint(label=label)
        except Exception:
            logger.debug("Failed to checkpoint after %s transition for %s", outcome, task.id, exc_info=True)

    async def _handle_worker_exception(self, task: "Task", reason: str) -> None:
        await self.team_run.task_center.fail(task.id, reason)
        conductor = getattr(self.team_run, "conductor", None)
        if conductor is None:
            return
        blocker = conductor.blocker_for_fix_task(task.id)
        if blocker is not None:
            await conductor.on_fix_failed(blocker.id, reason)

    _DEADLOCK_IDLE_THRESHOLD = 100  # ~5s of idle polls (100 * 50ms)

    async def _check_deadlock(self) -> bool:
        """Return True if all remaining tasks are stuck (no ready, no running)."""
        active = getattr(self.team_run, "_active_agent_runs", {})
        if active:
            return False
        try:
            statuses = await self.team_run.task_center._store.get_statuses()
        except Exception:
            return False
        has_pending = any(s in ("pending", "ready") for s in statuses.values())
        has_running = any(s == "running" for s in statuses.values())
        return has_pending and not has_running

    async def run_forever(self) -> None:
        tc = self.team_run.task_center
        dq = self.team_run.dispatch_queue
        pop_ready = dq.pop_ready
        idle_polls = 0
        while not self.team_run.cancel_event.is_set():
            try:
                rec = await pop_ready(self.team_run.id)
            except Exception as exc:
                logger.exception("DispatchQueue pop_ready failed: %s", exc)
                await asyncio.sleep(0.2)
                continue
            if rec is None:
                idle_polls += 1
                if idle_polls >= self._DEADLOCK_IDLE_THRESHOLD and await self._check_deadlock():
                    logger.error("Deadlock detected: pending tasks remain but none are ready or running")
                    self.team_run.cancel_event.set()
                    break
                await asyncio.sleep(0.05)
                continue
            idle_polls = 0
            task = _record_to_task(rec)
            tc.graph[task.id] = task
            try:
                await self._run_one(task)
            except Exception as exc:
                logger.exception("Worker error on %s: %s", task.id, exc)
                await self._handle_worker_exception(task, f"worker_exception: {exc}")

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
            await self._handle_worker_exception(task, f"runner_exception: {exc}")
            return
        finally:
            unregister_agent_run = getattr(self.team_run, "unregister_agent_run", None)
            if callable(unregister_agent_run):
                unregister_agent_run(task.id, runner_task)

        current = await _current_task_state()
        if current is not None and current.status == TaskStatus.PAUSED:
            logger.info("Task %s completed after pause; ignoring stale result", task.id)
            return

        result = await self._run_post_run(task, defn, ctx)
        await self._dispatch(task, result)

    async def _inject_scope_warnings(self, task: "Task") -> None:
        await self.scope_notifier.inject_warning(task)

    async def _build_context(self, defn: "AgentDefinition", task: "Task") -> TeamAgentContext:
        if self.build_query_context is not None:
            return await self.build_query_context(defn, self.team_run, task)
        from team.runtime.context_builder import build_query_context
        return await build_query_context(defn, self.team_run, task)

    async def _plan_health_prefix(self, task: "Task") -> str | None:
        return await self.plan_health.compute_prefix(task)

    async def _run_post_run(
        self,
        task: "Task",
        defn: "AgentDefinition",
        ctx: TeamAgentContext,
    ) -> AgentResult | ReplanRequest | BlockerDeclaration:
        """Post-run phase: re-prompt agent with posthook tools only.

        The agent is re-prompted with ONLY its posthook tools and must call
        one of them. Loops up to 5 tool calls until a successful submission.
        """
        from external_trigger.runner import run as run_trigger
        from tools.posthook.toolkit import PosthookTools

        api_client = getattr(self.team_run, "api_client", None)
        if api_client is None:
            logger.warning("No api_client for post-run on task %s; returning empty result", task.id)
            return AgentResult(summary="completed (no api_client for posthook)")

        conductor = getattr(self.team_run, "conductor", None)
        messages: list[dict] = []
        if conductor is not None:
            messages = conductor._executor_snapshots.get(task.id, [])

        toolkit = PosthookTools.from_context(ctx)
        post_run_tools = toolkit.list_tools()
        if not post_run_tools:
            logger.warning("No posthook tools for task %s; returning empty result", task.id)
            return AgentResult(summary="completed (no posthook tools)")
        tool_summary = ", ".join(f"{t.name}: {t.description}" for t in post_run_tools)
        work_result = str(ctx.tool_metadata.get("work_result") or "").strip()
        if work_result:
            deterministic = self._deterministic_result(
                {t.name for t in post_run_tools}, work_result
            )
            if deterministic is not None:
                logger.info(
                    "[posthook] deterministic payload for task %s (%s)", task.id, task.agent_name,
                )
                return deterministic
        handoff = ""
        if work_result:
            handoff = (
                "\nUse the prior work result below as the canonical output from the main run. "
                "If it already contains valid plan or replan JSON, submit that exact structure "
                "via the appropriate tool instead of inventing a new one.\n\n"
                f"{work_result[:20_000]}"
            )

        posthook_prompt = str(ctx.tool_metadata.get("posthook_prompt") or "").strip()
        if not posthook_prompt:
            posthook_prompt = (
                "Your main work is complete. Submit your results by calling one of: "
                f"{tool_summary}. If your prior output already contains structured "
                "JSON matching a tool's schema (e.g. a plan with a 'tasks' array), "
                "pass that exact payload as the tool arguments — never call the tool "
                "with empty or placeholder input."
            )

        posthook_cwd = Path(
            str(
                ctx.tool_metadata.get("daytona_cwd")
                or ctx.tool_metadata.get("cwd")
                or "."
            )
        )
        execution_context = ToolExecutionContext(
            cwd=posthook_cwd,
            metadata=ctx.tool_metadata,
        )

        try:
            result = await run_trigger(
                agent_name=f"posthook:{task.agent_name}:{task.id}",
                messages=messages,
                system_prompt=(
                    "You are a task submission assistant. "
                    "The conversation history is frozen. Do not continue the task itself; "
                    "focus only on producing a valid terminal submission. "
                    "If a submission tool returns an error, revise the submission payload "
                    "to satisfy that error and retry."
                ),
                prompt=f"{posthook_prompt}{handoff}",
                tools=post_run_tools,
                api_client=api_client,
                max_tokens_per_turn=1000,
                execution_context=execution_context,
                execute_tools=True,
            )
        except Exception as exc:
            msg = f"[posthook] FAILED for task {task.id} ({task.agent_name}): {exc}"
            print(msg, file=sys.stdout, flush=True)
            logger.warning(msg, exc_info=True)
            return AgentResult(summary="completed (posthook runner failed)")

        msg = f"[posthook] completed for task {task.id} ({task.agent_name}): tool={result.tool_name} turns={result.turns_used}"
        print(msg, file=sys.stdout, flush=True)
        logger.info(msg)

        # Map RunResult → domain objects
        # Prefer resolved objects stashed in tool_result.metadata by the
        # posthook tools (roster-resolved, already validated) to avoid a
        # lossy re-parse of raw LLM input that drops roster resolution
        # and can fail task_center's second validation pass.
        tool_input = result.tool_input
        _tool_result = getattr(result, "tool_result", None)
        _meta = getattr(_tool_result, "metadata", None) or {}
        match result.tool_name:
            case "post_note":
                return AgentResult(summary=tool_input.get("content", ""))
            case "submit_plan":
                plan = _meta.get("resolved_plan")
                if plan is None:
                    plan = Plan.from_dict(tool_input)
                return AgentResult(summary="", submitted_plan=plan)
            case "add_tasks" | "cancel_and_redraft":
                replan = _meta.get("resolved_replan")
                if replan is None:
                    replan = ReplanPlan.from_dict(tool_input)
                return AgentResult(summary="", submitted_replan=replan)
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
    def _deterministic_result(
        tool_names: set[str], work_result: str,
    ) -> "AgentResult | ReplanRequest | BlockerDeclaration | None":
        """Build a terminal result directly from JSON embedded in ``work_result``.

        Short-circuits the posthook LLM when the agent's final text already
        contains a parseable submission payload.
        """
        payload = _extract_json_payload(work_result)
        if payload is None:
            return None
        if "submit_plan" in tool_names and isinstance(payload.get("tasks"), list):
            try:
                return AgentResult(summary="", submitted_plan=Plan.from_dict(payload))
            except Exception:
                return None
        if "add_tasks" in tool_names and "add_tasks" in payload:
            try:
                return AgentResult(summary="", submitted_replan=ReplanPlan.from_dict(payload))
            except Exception:
                return None
        if (
            "declare_blocker" in tool_names
            and "root_cause_paths" in payload
            and "reason" in payload
        ):
            return BlockerDeclaration(
                root_cause_paths=list(payload.get("root_cause_paths") or []),
                reason=str(payload.get("reason") or ""),
                suggestion=payload.get("suggestion"),
            )
        return None

    async def _post_completion_note(self, task: "Task", summary: str) -> None:
        if not summary or summary.startswith("completed ("):
            return
        budget = getattr(self.team_run, "budgets", None)
        max_bytes = getattr(budget, "max_note_bytes", 100_000) if budget else 100_000
        from team.models import Note
        try:
            await self.team_run.task_center.notes.post(Note(
                id=str(uuid.uuid4()), task_id=task.id,
                agent_name=task.agent_name or "unknown",
                content=summary[:max_bytes], timestamp=time.time(),
                paths=list(task.scope_paths) if task.scope_paths else [],
                tags=["implementation"],
            ))
        except Exception:
            logger.debug("completion note: post failed for %s", task.id, exc_info=True)

    async def _post_checkpoint_note(self, task: "Task", result: Any) -> str | None:
        return await self.plan_health.post_checkpoint_note(task, result)

    async def _dispatch(self, task: "Task", result: Any) -> None:
        tc = self.team_run.task_center
        conductor = getattr(self.team_run, "conductor", None)
        fix_blocker = conductor.blocker_for_fix_task(task.id) if conductor is not None else None
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
