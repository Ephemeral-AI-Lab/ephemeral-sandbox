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
QueryContextBuilder = Callable[["AgentDefinition", "TeamRun", "Task"], Awaitable[TeamAgentContext]]


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
            except Exception as exc:
                logger.exception("Dispatcher pop_ready failed: %s", exc)
                await asyncio.sleep(0.2)
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
        await self._inject_scope_warnings(task)

        ctx = await self._build_context(defn, task)

        # Inject plan health prefix into user_message (Priority 0: never trimmed).
        # This bypasses context_for budget/priority — the agent always sees it.
        health_prefix = await self._plan_health_prefix(task)
        if health_prefix:
            ctx.user_message = health_prefix + "\n\n" + ctx.user_message

        # Subscribe to real-time scope change notifications when available.
        listener = getattr(self.team_run, "scope_listener", None)
        if (
            listener is not None
            and getattr(listener, "is_running", False)
            and task.scope_paths
        ):
            from team.runtime.scope_change_buffer import ScopeChangeBuffer

            scope_buffer = ScopeChangeBuffer()
            listener.subscribe(agent_run_id, list(task.scope_paths), scope_buffer)
            if ctx.tool_metadata is not None:
                ctx.tool_metadata.extras["scope_change_buffer"] = scope_buffer

        try:
            await self.runner(defn, ctx)
        except Exception as exc:
            await dispatcher.fail(task_id, f"runner_exception: {exc}")
            return
        finally:
            if listener is not None:
                listener.unsubscribe(agent_run_id)

        result = self._posthook(ctx, defn)

        # Post-run: check if scope drifted during execution.
        # If the agent succeeded but scope changed and it never checked,
        # tag the result so downstream tasks know.
        result = await self._tag_if_scope_drifted(task, result, ctx)

        await self._dispatch(task_id, task, result)

    async def _inject_scope_warnings(self, task: "Task") -> None:
        """Check if files in task's scope changed since plan creation.

        If external changes are detected, inject a warning note into the
        Task Center so the agent sees it in context_for(). The agent
        decides whether to proceed or request_replan()."""
        if not task.scope_paths:
            return
        store = getattr(self.team_run, "file_change_store", None)
        if store is None or not getattr(store, "initialized", False):
            return
        created_ts = task.created_at.timestamp() if task.created_at else 0.0
        changes = store.changes_since(created_ts)
        # Filter to scope and exclude changes by this task's own agent run
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
            lines.append(f"- {e.file_path} ({e.edit_type} by {e.agent_id}, "
                         f"{int(now - e.created_at.timestamp())}s ago)")
        lines.append("Review these changes before proceeding. "
                      "Call request_replan() if your task is no longer valid.")
        from team.models import Note
        try:
            await self.team_run.task_center.post(
                Note(
                    id=str(uuid.uuid4()),
                    task_id=task.id,
                    agent_name="system",
                    content="\n".join(lines),
                    timestamp=now,
                    scope_paths=list(task.scope_paths),
                )
            )
        except Exception:
            logger.debug("Failed to persist scope warning for %s", task.id, exc_info=True)

    async def _build_context(self, defn: "AgentDefinition", task: "Task") -> TeamAgentContext:
        """Build agent context using an override when provided."""
        if self.build_query_context is not None:
            return await self.build_query_context(defn, self.team_run, task)
        from team.runtime.context_builder import build_query_context
        return await build_query_context(defn, self.team_run, task)

    async def _plan_health_prefix(self, task: "Task") -> str | None:
        """Build a plan-health prefix for the agent's user_message.

        Unlike checkpoint notes (which flow through Priority 4 parent chain
        and can be truncated), this prefix is prepended directly to the
        user_message — it's never trimmed by context_for's budget logic.

        Returns None if plan health is normal.
        """
        if not task.parent_id:
            return None
        try:
            stats = await self.team_run.dispatcher.sibling_stats(task.parent_id)
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

    async def _tag_if_scope_drifted(
        self,
        task: "Task",
        result: Any,
        ctx: TeamAgentContext,
    ) -> Any:
        """Check if the agent's scope drifted during execution.

        If the agent produced a successful result (AgentResult) but files
        in its scope were modified by other agents since the task started,
        and it never called context_changed_since (tracked via metadata),
        append a drift warning to its summary. Downstream tasks reading
        the summary note will see the warning.

        Returns the result unchanged if no drift, or a tagged result.
        """
        if not isinstance(result, AgentResult) or not task.scope_paths:
            return result
        if not task.started_at:
            return result

        # Check if agent already verified freshness (opted in to awareness)
        if ctx.tool_metadata.get("checked_context_freshness"):
            return result

        fc_store = getattr(self.team_run, "file_change_store", None)
        if fc_store is None or not getattr(fc_store, "initialized", False):
            return result

        try:
            started_ts = task.started_at.timestamp()
            changes = fc_store.changes_since(started_ts)
            external = [
                e for e in changes
                if e.agent_run_id != (task.agent_run_id or "")
                and any(e.file_path.startswith(p.rstrip("/")) for p in task.scope_paths)
            ]
        except Exception:
            return result

        if not external:
            return result

        drift_files = [e.file_path for e in external[:5]]
        warning = (
            f"\n\n[DRIFT WARNING: {len(external)} file(s) in scope were "
            f"modified by other agents during execution: "
            f"{', '.join(drift_files)}. Results may be based on stale state.]"
        )
        return AgentResult(
            summary=(result.summary or "") + warning,
            submitted_plan=result.submitted_plan,
            submitted_replan=result.submitted_replan,
        )

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

    _extract_result = _posthook

    async def _post_completion_note(self, task: "Task", summary: str) -> None:
        """Auto-post the agent's work summary as a Task Center note.

        Posted with the completing task's own task_id so downstream
        dependents see it via the dep filter in context_for().
        Truncated to max_note_bytes to respect budget limits.
        """
        if not summary or summary in (
            "completed (no explicit submission)",
            "planner_did_not_submit_plan",
        ):
            return
        budget = getattr(self.team_run, "budgets", None)
        max_bytes = getattr(budget, "max_note_bytes", 100_000) if budget else 100_000
        truncated = summary[:max_bytes]
        from team.models import Note
        try:
            await self.team_run.task_center.post(
                Note(
                    id=str(uuid.uuid4()),
                    task_id=task.id,
                    agent_name=task.agent_name or "unknown",
                    content=truncated,
                    timestamp=time.time(),
                    scope_paths=list(task.scope_paths) if task.scope_paths else [],
                )
            )
        except Exception:
            logger.debug("completion note: post failed for %s", task.id, exc_info=True)

    async def _post_checkpoint_note(self, task: "Task", result: Any) -> str | None:
        """Post a checkpoint note after task completion.

        Surfaces plan-level health signals (failure rate, retry storms)
        into the Task Center so replanners and siblings see them.
        Returns ``"replan"`` if plan health is critical, else ``None``.
        No LLM call — pure arithmetic on task statuses.

        The note is posted with ``task_id=task.parent_id`` (the parent,
        not the completed task itself). This is critical for read-path
        visibility:

        - **Avoids shadowing** the task's own ``submit_summary()`` note.
          ``context_for`` deduplicates dep notes to latest-per-task_id.
          Posting with the task's own id would overwrite the work summary.
        - **Parent chain visibility.** ``context_for`` walks the parent
          chain (Priority 4). Notes attributed to the parent are visible
          to all tasks that share that parent — including replanners,
          which are inserted at the same parent_id/depth.
        - **search_context visibility.** Notes are scope-indexed, so any
          agent calling ``search_context`` with overlapping scope will
          find the checkpoint note via FTS.
        """
        dispatcher = self.team_run.dispatcher
        try:
            stats = await dispatcher.sibling_stats(task.parent_id)
        except Exception:
            logger.debug("checkpoint note: sibling_stats failed for %s", task.id, exc_info=True)
            return None

        # Build note content
        lines = [f"**Checkpoint: {task.id} ({task.agent_name}) → {task.status}**"]

        if task.failure_reason:
            lines.append(f"Failure: {task.failure_reason}")

        # Files touched by this agent run
        fc_store = getattr(self.team_run, "file_change_store", None)
        if fc_store is not None and getattr(fc_store, "initialized", False) and task.agent_run_id:
            try:
                changes = fc_store.changes_by_agent_run(self.team_run.id, task.agent_run_id)
                if changes:
                    paths = [c.file_path for c in changes[:10]]
                    lines.append(f"Files touched: {', '.join(paths)}")
            except Exception:
                pass

        # Plan health signals
        action = None
        done = stats.get("done", 0)
        failed = stats.get("failed", 0)
        started = done + failed
        retry_total = stats.get("retry_total", 0)

        if started >= 3 and started > 0 and failed / started > 0.4:
            lines.append(
                f"PLAN HEALTH CRITICAL: {failed}/{started} sibling tasks failed"
            )
            action = "replan"
        elif retry_total >= 3:
            lines.append(
                f"PLAN HEALTH WARNING: {retry_total} retries across sibling tasks"
            )

        # Post with task_id=parent_id so it flows through parent chain
        # reads (Priority 4), not dep reads (avoids shadowing submit_summary() notes).
        note_owner = task.parent_id or task.id
        from team.models import Note
        try:
            await self.team_run.task_center.post(
                Note(
                    id=str(uuid.uuid4()),
                    task_id=note_owner,
                    agent_name="checkpoint",
                    content="\n".join(lines),
                    timestamp=time.time(),
                    scope_paths=list(task.scope_paths) if task.scope_paths else [],
                )
            )
        except Exception:
            logger.debug("checkpoint note: post failed for %s", task.id, exc_info=True)

        return action

    async def _dispatch(self, task_id: str, task: "Task", result: Any) -> None:
        dispatcher = self.team_run.dispatcher
        if isinstance(result, RetryRequest):
            await dispatcher.retry_work_item(task_id, result)
            await self._checkpoint_after_transition(task, outcome="retry")
            await self._post_checkpoint_note(task, result)
            return
        if isinstance(result, ReplanRequest):
            await dispatcher.request_replan(task_id, result)
            await self._checkpoint_after_transition(task, outcome="replan_request")
            await self._post_checkpoint_note(task, result)
            return
        new_items = await dispatcher.complete(task_id, result)

        # Auto-post work summary as a Task Center note so sibling and
        # downstream tasks can read it via dep/scope filters without
        # requiring the agent to call post_note() explicitly.
        if isinstance(result, AgentResult) and result.summary:
            await self._post_completion_note(task, result.summary)

        if self.after_dispatch is not None:
            cb = self.after_dispatch(task, result, new_items)
            if isinstance(cb, Awaitable):
                await cb
        await self._checkpoint_after_transition(task, outcome="complete")

        # Post-completion: checkpoint note with plan health signals
        await self._post_checkpoint_note(task, result)
