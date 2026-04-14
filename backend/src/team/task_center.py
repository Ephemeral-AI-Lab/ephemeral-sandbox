"""TaskCenter — unified task lifecycle management.

Replaces the former Dispatcher + DispatcherStore + TaskCenter split.
Single source of truth for task structure, state, context, and notes.
DispatchQueue handles atomic task claiming separately.
"""

from __future__ import annotations

import asyncio
import copy
import logging
import time
import uuid
from collections import deque
from typing import TYPE_CHECKING, Any, Callable

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker  # noqa: F401 — used in __init__ type hint

from team.errors import BudgetExceeded, CheckpointNotFound, InvalidPlan
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Note,
    ReplanRequest,
    RetryRequest,
    Task,
    TaskSpec,
    TaskStatus,
    _utcnow,
)
from team.persistence.events import (
    TeamRunEvent,
    make_budget_update,
    make_checkpoint_taken,
    make_note_posted,
    make_task_added,
    make_task_status,
    task_to_dict,
)
from team.persistence.run_store import NullTeamRunStore, TeamRunStore
from team.persistence.task_record import TaskRecord  # noqa: F401 — re-exported
from team.persistence.task_store import TaskStore, record_to_task
from team.planning.validation import validate_plan  # noqa: F401 — used in complete_task
from team.runtime.checkpoint import TeamRunCheckpoint

if TYPE_CHECKING:
    pass

logger = logging.getLogger(__name__)

# Backward-compat re-exports for external callers
_record_to_task = record_to_task


def _note_preview(content: str, *, limit: int = 240) -> str:
    compact = " ".join(content.split())
    if len(compact) <= limit:
        return compact
    return compact[: limit - 1] + "…"


class TaskCenter:
    """Unified task lifecycle management.

    Owns task structure, state, context, and notes.
    PostgreSQL for task persistence, in-memory for notes.
    """

    def __init__(
        self,
        *,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        goal: str = "",
        user_request: str = "",
        file_change_store: Any = None,
        max_checkpoints: int = 10,
        event_store: TeamRunStore | None = None,
        checkpoint_store: Any = None,
    ) -> None:
        self._notes: list[Note] = []
        self.goal = goal
        self.user_request = user_request
        self._team_run_id = team_run_id
        self._store = TaskStore(session_factory, team_run_id)
        self._file_change_store = file_change_store
        self.budgets = budgets
        self.budget_state = budget_state
        self._events: TeamRunStore = event_store or NullTeamRunStore()
        self.graph: dict[str, Task] = {}
        self._ready_order: list[str] = []
        self._resume_snapshot: list[Task] | None = None
        self.lock = asyncio.Lock()
        self._checkpoints: deque[TeamRunCheckpoint] = deque(maxlen=max_checkpoints)
        self._checkpoint_seq = 0
        self._checkpoint_store = checkpoint_store

    # ---- activity tracking (active mode) -----------------------------------

    def _get_counters(self, task_id: str) -> dict[str, Any]:
        if not hasattr(self, '_activity_counters'):
            self._activity_counters: dict[str, dict[str, Any]] = {}
        if task_id not in self._activity_counters:
            self._activity_counters[task_id] = {"edits": 0, "turns": 0, "files_edited": []}
        return self._activity_counters[task_id]

    def on_edit(self, task_id: str, file_path: str) -> None:
        """Track an edit tool completion."""
        c = self._get_counters(task_id)
        c["edits"] += 1
        if file_path not in c["files_edited"]:
            c["files_edited"].append(file_path)

    def on_posthook(self, task_id: str) -> None:
        """Reset turn counter when posthook tool completes."""
        self._get_counters(task_id)["turns"] = 0

    def tick(self, task_id: str) -> None:
        """Increment turn counter after each tool result."""
        self._get_counters(task_id)["turns"] += 1

    def on_note_posted(self, note: Note) -> None:
        """Reset counters only for agent-authored or auto-generated task notes."""
        if note.agent_name in {"system", "checkpoint"}:
            return
        if hasattr(self, '_activity_counters') and note.task_id in self._activity_counters:
            self._activity_counters[note.task_id] = {"edits": 0, "turns": 0, "files_edited": []}

    def should_checkpoint(self, task_id: str) -> str | None:
        """Check if auto-checkpoint should fire. Returns trigger type or None."""
        c = self._get_counters(task_id)
        if c["edits"] >= 5:
            return "edit"
        if c["turns"] >= 10:
            return "turn"
        return None

    async def check(
        self,
        task_id: str,
        *,
        snapshot: list[dict] | None = None,
        api_client: Any = None,
        model: str | None = None,
    ) -> bool:
        """Spawn an EphemeralTask to generate a progress note if thresholds are crossed.

        If *snapshot* and *api_client* are provided, uses a single-shot LLM call
        to produce a rich note from the agent's conversation.  The prompt is sent
        as a **user message** (not a SystemNotification).

        Falls back to a factual counter-based note when no snapshot is available.
        """
        trigger = self.should_checkpoint(task_id)
        if trigger is None:
            return False
        task = self.graph.get(task_id)
        agent_name = task.agent_name if task else "unknown"
        scope_paths = list(task.scope_paths) if task and task.scope_paths else []
        c = self._get_counters(task_id)

        logger.info(
            "[task_center] auto-note trigger=%s task=%s agent=%s edits=%d turns=%d scope=%s",
            trigger,
            task_id,
            agent_name,
            c["edits"],
            c["turns"],
            ",".join(scope_paths) if scope_paths else "-",
        )

        content: str | None = None

        if snapshot and api_client:
            from external_trigger.tc_note import (
                EDIT_CHECKPOINT_PROMPT,
                TURN_CHECKPOINT_PROMPT,
                run_checkpoint_note,
            )
            prompt = EDIT_CHECKPOINT_PROMPT if trigger == "edit" else TURN_CHECKPOINT_PROMPT
            agent_run_id = task.agent_run_id or task_id if task else task_id
            result = await run_checkpoint_note(
                task_id=task_id,
                agent_run_id=agent_run_id,
                messages=snapshot,
                prompt=prompt,
                trigger=trigger,
                max_tokens=500,
                model=model,
                api_client=api_client,
            )
            if result.note_summary:
                content = result.note_summary

        # Fallback: factual note when no LLM available or LLM returned empty
        if content is None:
            if trigger == "edit":
                files = ", ".join(c["files_edited"][-10:])
                content = f"Auto-checkpoint ({c['edits']} edits): {files}"
            else:
                content = f"Auto-checkpoint: {c['turns']} turns without progress note"

        await self.post(Note(
            id=str(uuid.uuid4()),
            task_id=task_id,
            agent_name=f"{agent_name} (auto)",
            content=content,
            timestamp=time.time(),
            scope_paths=scope_paths,
        ))
        return True

    async def read_sibling_notes(
        self,
        parent_id: str,
        *,
        keyword: str | None = None,
        scope_paths: list[str] | None = None,
    ) -> str:
        """Read notes from sibling tasks under the same parent."""
        sibling_ids = await self._sibling_subtree_ids(parent_id)
        if not sibling_ids:
            return ""
        notes = await self.read(
            authors=sibling_ids,
            scope_paths=scope_paths,
        )
        if keyword:
            kw = keyword.lower()
            notes = [n for n in notes if kw in n.content.lower()]
        if not notes:
            return ""
        return self._render_notes("Sibling notes", notes)

    # ---- events & budget ---------------------------------------------------

    def _emit(self, event: TeamRunEvent) -> None:
        try:
            self._events.append(event)
        except Exception:
            logger.exception("team event store append failed; continuing")

    def _emit_budget(self) -> None:
        self._emit(make_budget_update(
            self._team_run_id,
            tasks_used=self.budget_state.tasks_used,
            note_bytes_used=self.budget_state.note_bytes_used,
            replans_used=self.budget_state.replans_used,
        ))

    def _charge_tasks(self, n: int = 1) -> None:
        self.budget_state.tasks_used += n
        self._emit_budget()

    def new_id(self) -> str:
        return str(uuid.uuid4())

    # ---- notes (in-memory) -------------------------------------------------

    @staticmethod
    def _matches_scope(note_scopes: list[str], query_scopes: list[str]) -> bool:
        if not note_scopes:
            return True
        normalized = [s.rstrip("/") for s in query_scopes if s]
        return any(
            TaskCenter._scope_overlaps(ns, qs)
            for ns in note_scopes for qs in normalized
        )

    @staticmethod
    def _scope_overlaps(note_scope: str, query_scope: str) -> bool:
        n, q = note_scope.rstrip("/"), query_scope.rstrip("/")
        if not n or not q:
            return False
        return n == q or n.startswith(q + "/") or q.startswith(n + "/")

    async def post(self, note: Note) -> None:
        self._notes.append(note)
        self.on_note_posted(note)
        auto_generated = note.agent_name.endswith(" (auto)")
        preview = _note_preview(note.content)
        logger.info(
            "[task_center] %snote task=%s agent=%s scope=%s preview=%s",
            "auto-" if auto_generated else "",
            note.task_id,
            note.agent_name,
            ",".join(note.scope_paths) if note.scope_paths else "-",
            preview,
        )
        self._emit(make_note_posted(
            self._team_run_id,
            task_id=note.task_id,
            agent_name=note.agent_name,
            auto=auto_generated,
            scope_paths=note.scope_paths,
            content_preview=preview,
            content_bytes=len(note.content.encode("utf-8")),
        ))

    async def read(
        self,
        *,
        authors: list[str] | None = None,
        scope_paths: list[str] | None = None,
        since: float | None = None,
        limit: int | None = None,
    ) -> list[Note]:
        results = list(self._notes)
        if authors:
            s = set(authors)
            results = [n for n in results if n.task_id in s]
        if scope_paths:
            results = [n for n in results if self._matches_scope(n.scope_paths, scope_paths)]
        if since is not None:
            results = [n for n in results if n.timestamp >= since]
        if limit is not None and limit > 0:
            results = results[-limit:]
        return results

    async def read_notes(
        self,
        *,
        task_id: str,
        scope: str = "full",
        keyword: str | None = None,
        scope_paths: list[str] | None = None,
        limit: int | None = None,
    ) -> list[Note]:
        """Read notes with scope filtering.

        Scopes:
            full — entire task center
            siblings — sibling tasks and their children
        """
        if scope == "full":
            notes = await self.read(scope_paths=scope_paths, limit=limit)
        elif scope == "siblings":
            task = await self.get_task(task_id)
            if task is None:
                return []
            sibling_ids = await self._sibling_subtree_ids(task.parent_id)
            sibling_ids = [tid for tid in sibling_ids if tid != task_id]
            notes = await self.read(
                authors=sibling_ids,
                scope_paths=scope_paths,
                limit=limit,
            )
        else:
            notes = await self.read(scope_paths=scope_paths, limit=limit)
        if keyword:
            kw = keyword.lower()
            notes = [n for n in notes if kw in n.content.lower()]
        return notes

    async def context_for(
        self,
        task: Task,
        *,
        max_context_bytes: int = 200_000,
    ) -> str:
        """Build context string for a task. No external callbacks needed."""
        budget = max_context_bytes
        sections: list[str] = []

        if task.retry_count and task.retry_count > 0:
            s = (
                f"## ⚠ RETRY #{task.retry_count} of {task.max_retries}\n"
                f"Your previous attempt at this task failed. "
                f"Do NOT repeat the same approach — read the retry notes below "
                f"for what went wrong."
            )
            if task.retry_count >= task.max_retries:
                s += (
                    f"\n\n**This is your LAST attempt.** If you cannot fix the "
                    f"issue with a different approach, call `request_replan()` "
                    f"with a clear diagnostic so the replanner can restructure the work."
                )
            sections.append(s)
            budget -= len(s.encode())

        task_section = f"## Your task\n{task.task}"
        if task.scope_paths:
            task_section += f"\n\nScope: {', '.join(task.scope_paths)}"
        sections.append(task_section)
        budget -= len(task_section.encode())

        if task.retry_count and task.retry_count > 0 and budget > 0:
            self_notes = await self.read(authors=[task.id])
            if self_notes:
                sec = self._render_notes("Previous attempt context", self_notes)
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b
                else:
                    sections.append(self._truncate_section("Previous attempt context", self_notes, budget))
                    budget = 0

        if task.deps and budget > 0:
            dep_notes = await self.read(authors=task.deps)
            if dep_notes:
                by_dep: dict[str, Note] = {}
                for n in dep_notes:
                    by_dep[n.task_id] = n
                deduped = list(by_dep.values())
                sec = self._render_notes("Context from dependencies", deduped)
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b
                else:
                    sections.append(self._truncate_section("Context from dependencies", deduped, budget))
                    budget = 0

        fcs = self._file_change_store
        if fcs is not None and getattr(fcs, "initialized", False) and budget > 0 and task.scope_paths:
            created_ts = task.created_at.timestamp() if task.created_at else 0.0
            changes = fcs.changes_since(created_ts)
            scoped = [
                e for e in changes
                if any(e.file_path.startswith(p.rstrip("/")) for p in task.scope_paths)
            ]
            if scoped:
                now = time.time()
                lines = [
                    f"- {e.file_path} ({e.edit_type} by {e.agent_id}, "
                    f"{int(now - e.created_at.timestamp())}s ago)"
                    for e in scoped
                ]
                sec = "## Recent changes in your scope\n" + "\n".join(lines)
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b

        if task.parent_id and budget > 0:
            parent_ids = await self._parent_chain_ids(task)
            parent_notes = await self.read(authors=parent_ids)
            if parent_notes:
                sec = self._render_notes("Parent context", parent_notes)
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b
                else:
                    sections.append(self._truncate_section("Parent context", parent_notes, budget))

        return "\n\n".join(sections)

    def snapshot(self) -> list[Note]:
        return list(self._notes)

    def restore(self, notes: list[Note]) -> None:
        self._notes = list(notes)

    def _render_notes(self, header: str, notes: list[Note]) -> str:
        lines = [f"## {header}"]
        for n in notes:
            lines.append(f"### {n.agent_name} ({n.task_id})")
            lines.append(n.content)
        return "\n".join(lines)

    def _truncate_section(self, header: str, notes: list[Note], budget: int) -> str:
        sep = "\n"
        header_line = f"## {header}"
        remaining = budget - len(header_line.encode()) - len(sep.encode())
        lines = [header_line]
        for n in notes:
            entry = f"### {n.agent_name} ({n.task_id})\n{n.content}"
            cost = len(entry.encode()) + len(sep.encode())
            if cost <= remaining:
                lines.append(entry)
                remaining -= cost
            else:
                safe = max(0, remaining - len(sep.encode()) - len("\n...[truncated]".encode()))
                lines.append(entry.encode()[:safe].decode("utf-8", errors="ignore") + "\n...[truncated]")
                break
        return sep.join(lines)

    async def _parent_chain_ids(self, task: Task) -> list[str]:
        if task.parent_id is None:
            return []
        parent_ids: list[str] = []
        seen: set[str] = set()
        current_id = task.parent_id
        while current_id and current_id not in seen:
            parent_ids.append(current_id)
            seen.add(current_id)
            parent = await self.get_task(current_id)
            current_id = parent.parent_id if parent is not None else None
        return parent_ids

    # ---- SQL delegations (persistence in TaskStore) -------------------------

    async def get_task(self, task_id: str) -> Task | None:
        rec = await self._store.get_record(task_id)
        if rec is None:
            self.graph.pop(task_id, None)
            return None
        task = record_to_task(rec)
        self.graph[task.id] = task
        return task

    async def _get_record(self, task_id: str) -> TaskRecord | None:
        return await self._store.get_record(task_id)

    async def get_all_tasks(self) -> list[TaskRecord]:
        return await self._store.get_all_tasks()

    async def get_adjacency(self) -> dict[str, list[str]]:
        return await self._store.get_adjacency()

    async def get_statuses(self) -> dict[str, str]:
        return await self._store.get_statuses()

    async def get_task_ids(self) -> set[str]:
        return await self._store.get_task_ids()

    async def get_done_sibling_ids(
        self, *, task_id: str, parent_id: str | None, since: float | None = None,
    ) -> list[str]:
        return await self._store.get_done_sibling_ids(
            task_id=task_id, parent_id=parent_id, since=since,
        )

    async def all_terminal(self) -> bool:
        return await self._store.all_terminal()

    async def sibling_stats(self, parent_id: str | None) -> dict[str, int]:
        return await self._store.sibling_stats(parent_id)

    async def _mark_done(self, task_id: str) -> list[str]:
        return await self._store.mark_done(task_id)

    async def _mark_expanded(self, task_id: str) -> None:
        return await self._store.mark_expanded(task_id)

    async def _maybe_promote_expanded_parent(self, child_id: str) -> list[str]:
        return await self._store.maybe_promote_expanded_parent(child_id)

    async def _mark_terminal(self, task_id: str, status: str, reason: str) -> None:
        return await self._store.mark_terminal(task_id, status, reason)

    async def insert_plan(
        self, specs: list[TaskSpec], parent_id: str | None = None,
        parent_depth: int = 0, parent_root_id: str | None = None,
    ) -> list[TaskRecord]:
        return await self._store.insert_plan(specs, parent_id, parent_depth, parent_root_id)

    async def cascade_cancel_recursive(self, root_task_id: str) -> list[str]:
        return await self._store.cascade_cancel_recursive(root_task_id)

    async def _fail_task_sql(self, task_id: str, reason: str) -> list[tuple[str, str]]:
        return await self._store.fail_task(task_id, reason)

    async def _retry_task_sql(self, task_id: str, max_retries: int) -> bool:
        return await self._store.retry_task(task_id, max_retries)

    async def cancel_all_pending(self) -> int:
        return await self._store.cancel_all_pending()

    async def cancel_all_running(self, reason: str) -> int:
        return await self._store.cancel_all_running(reason)

    async def pause_running_task(
        self, task_id: str, blocker_id: str, checkpoint: str, verdict: str,
    ) -> bool:
        return await self._store.pause_running_task(task_id, blocker_id, checkpoint, verdict)

    async def resume_paused_tasks(self, blocker_id: str) -> int:
        return await self._store.resume_paused_tasks(blocker_id)

    async def cancel_paused_tasks(self, blocker_id: str) -> int:
        return await self._store.cancel_paused_tasks(blocker_id)

    async def get_siblings_and_descendants(self, initiating_task_id: str) -> list[TaskRecord]:
        return await self._store.get_siblings_and_descendants(initiating_task_id)

    async def _sibling_subtree_ids(self, parent_id: str | None) -> list[str]:
        return await self._store.sibling_subtree_ids(parent_id)

    async def _request_replan_sql(
        self, task_id: str, reason: str, suggestion: str | None, replanner_agent: str,
    ) -> TaskRecord:
        return await self._store.request_replan(task_id, reason, suggestion, replanner_agent)

    async def cancel_by_ids(self, task_ids: list[str], reason: str) -> int:
        return await self._store.cancel_by_ids(task_ids, reason)

    async def mark_running(self, task_id: str, agent_run_id: str) -> Task:
        rec = await self._store.mark_running_sql(task_id, agent_run_id)
        if rec is None:
            raise RuntimeError(f"mark_running: {task_id} not found")
        task = record_to_task(rec)
        self.graph[task.id] = task
        self._emit(make_task_status(
            self._team_run_id, task_id, "running",
            agent_run_id=agent_run_id,
            started_at=task.started_at.isoformat() if task.started_at else None,
        ))
        return task

    async def recover_running(self) -> list[TaskRecord]:
        return await self._store.recover_running()

    async def replace_run_tasks(self, tasks: list[Task]) -> None:
        return await self._store.replace_run_tasks(tasks)

    # ---- orchestration -----------------------------------------------------

    async def refresh_graph(self) -> dict[str, Task]:
        records = await self.get_all_tasks()
        self.graph = {r.id: _record_to_task(r) for r in records}
        self._ready_order = [r.id for r in records if r.status == "ready"]
        return self.graph

    async def add_task(self, t: Task) -> None:
        if self.budget_state.tasks_used >= self.budgets.max_tasks:
            raise BudgetExceeded(f"max_tasks={self.budgets.max_tasks} reached")
        await self.insert_plan(
            [TaskSpec(id=t.id, task=t.task, agent=t.agent_name,
                      deps=list(t.deps), scope_paths=list(t.scope_paths),
                      cascade_policy=t.cascade_policy)],
            parent_id=t.parent_id,
            parent_depth=max(0, t.depth - 1) if t.parent_id else 0,
            parent_root_id=t.root_id or None,
        )
        self.budget_state.tasks_used += 1
        t.status = TaskStatus.READY if not t.deps else TaskStatus.PENDING
        self.graph[t.id] = t
        if t.status == TaskStatus.READY and t.id not in self._ready_order:
            self._ready_order.append(t.id)
        self._emit(make_task_added(self._team_run_id, task_to_dict(t)))
        self._emit_budget()

    async def _mark_failed_and_cascade(self, task_id: str, reason: str) -> None:
        await self._mark_terminal(task_id, "failed", reason)
        await self.cascade_cancel_recursive(task_id)
        await self.refresh_graph()

    async def complete_task(self, task_id: str, result: AgentResult) -> list[Task]:
        new_items: list[Task] = []
        rec = await self._get_record(task_id)
        if rec is None or rec.status != "running":
            raise RuntimeError(f"complete: {task_id} is {rec.status if rec else 'missing'}, not RUNNING")

        from agents.registry import has_role as _has_role
        if _has_role(rec.agent_name, "planner") and result.submitted_plan is None:
            await self._mark_failed_and_cascade(task_id, "InvalidPlan: expandable task did not submit a plan")
            return []

        if result.submitted_plan is not None:
            new_depth = (rec.depth or 0) + 1
            if new_depth > self.budgets.max_depth:
                await self._mark_failed_and_cascade(
                    task_id,
                    f"InvalidPlan: plan would exceed max_depth={self.budgets.max_depth} "
                    f"(current depth={rec.depth or 0}). Planners at the depth limit must "
                    f"emit developer tasks with broader scopes instead of nested team_planner tasks.",
                )
                return []
            adj = await self.get_adjacency()
            allow_empty = bool(rec.root_id) and task_id != (rec.root_id or task_id)
            issues = validate_plan(
                result.submitted_plan, max_plan_size=self.budgets.max_plan_size,
                allow_empty=allow_empty, known_external_deps=set(adj.keys()),
            )
            if issues:
                await self._mark_failed_and_cascade(task_id, "InvalidPlan: " + "; ".join(i["msg"] for i in issues))
                return []
            local_to_global: dict[str, str] = {
                spec.id: self.new_id() for spec in result.submitted_plan.tasks if spec.id
            }
            specs: list[TaskSpec] = []
            for spec in result.submitted_plan.tasks:
                nid = local_to_global.get(spec.id) or self.new_id()
                rdeps = [local_to_global[d] if d in local_to_global else d for d in spec.deps]
                specs.append(TaskSpec(id=nid, task=spec.task, agent=spec.agent,
                                     deps=rdeps, scope_paths=list(spec.scope_paths),
                                     cascade_policy=spec.cascade_policy))
                new_items.append(Task(
                    id=nid, team_run_id=self._team_run_id, agent_name=spec.agent,
                    status=TaskStatus.READY if not rdeps else TaskStatus.PENDING,
                    task=spec.task, deps=rdeps, scope_paths=list(spec.scope_paths),
                    cascade_policy=spec.cascade_policy, parent_id=task_id,
                    root_id=rec.root_id or task_id, depth=new_depth,
                ))
            if self.budget_state.tasks_used + len(new_items) > self.budgets.max_tasks:
                await self._mark_failed_and_cascade(task_id, "BudgetExceeded: max_tasks")
                return []
            await self.insert_plan(specs, parent_id=task_id, parent_depth=rec.depth or 0,
                                   parent_root_id=rec.root_id or task_id)
            self.budget_state.tasks_used += len(new_items)
            for t in new_items:
                self._emit(make_task_added(self._team_run_id, task_to_dict(t)))
            self._emit_budget()

        if result.submitted_plan is not None:
            await self._mark_expanded(task_id)
            self._emit(make_task_status(self._team_run_id, task_id, "expanded", finished_at=_utcnow().isoformat()))
        else:
            await self._mark_done(task_id)
            self._emit(make_task_status(self._team_run_id, task_id, "done", finished_at=_utcnow().isoformat()))
            for pid in await self._maybe_promote_expanded_parent(task_id):
                self._emit(make_task_status(self._team_run_id, pid, "done", finished_at=_utcnow().isoformat()))

        if result.submitted_replan is not None:
            await self.apply_replan(
                replan_task_id=task_id, add_tasks=result.submitted_replan.add_tasks,
                cancel_ids=result.submitted_replan.cancel_ids,
                target_depth=rec.depth or 0, target_parent_id=rec.parent_id,
                target_root_id=rec.root_id or "",
            )
        await self.refresh_graph()
        return new_items

    async def fail(self, task_id: str, reason: str) -> None:
        warnings = await self._fail_task_sql(task_id, reason)
        for dep_id, msg in warnings:
            try:
                await self.post(Note(id=self.new_id(), task_id=dep_id, agent_name="system", content=msg))
            except Exception:
                logger.debug("Failed to post warning note for %s", dep_id, exc_info=True)
        await self.refresh_graph()

    async def retry_task(self, task_id: str, request: RetryRequest) -> None:
        rec = await self._get_record(task_id)
        if rec is None:
            raise RuntimeError(f"retry: {task_id} not found")
        success = await self._retry_task_sql(task_id, rec.max_retries)
        await self.refresh_graph()
        if not success:
            self._emit(make_task_status(self._team_run_id, task_id, "failed", failure_reason="retry_exhausted"))

    async def request_replan(self, task_id: str, request: ReplanRequest) -> Task:
        if self.budget_state.replans_used >= self.budgets.max_replans_per_run:
            raise BudgetExceeded("max_replans_per_run reached")
        from agents.registry import find_by_role
        replanners = find_by_role("replanner")
        if not replanners:
            raise RuntimeError("no agent with role='replanner' is registered")
        rec = await self._request_replan_sql(task_id, reason=request.reason,
                                             suggestion=request.suggestion,
                                             replanner_agent=replanners[0].name)
        self.budget_state.tasks_used += 1
        self.budget_state.replans_used += 1
        task = _record_to_task(rec)
        self._emit(make_task_added(self._team_run_id, task_to_dict(task)))
        self._emit_budget()
        await self.refresh_graph()
        return task

    async def apply_replan(
        self, replan_task_id: str, add_tasks: list[TaskSpec], cancel_ids: list[str],
        target_depth: int, target_parent_id: str | None, target_root_id: str,
    ) -> dict[str, int]:
        from team.planning.validation import _has_cycle
        for cid in cancel_ids:
            rec = await self._get_record(cid)
            if rec is None:
                raise InvalidPlan(f"cancel target {cid} not found")
            if rec.parent_id != target_parent_id:
                raise InvalidPlan(f"cancel target {cid} has parent {rec.parent_id!r}, but replan scoped to {target_parent_id!r}")
            if rec.status not in ("pending", "ready", "expanded"):
                raise InvalidPlan(f"cancel target {cid} is {rec.status}; can only cancel PENDING, READY, or EXPANDED")
        local_to_new: dict[str, str] = {}
        for spec in add_tasks:
            if spec.id:
                if spec.id in local_to_new:
                    raise InvalidPlan(f"duplicate id '{spec.id}'")
                local_to_new[spec.id] = self.new_id()
        adj = await self.get_adjacency()
        clean_adj = {k: v for k, v in adj.items() if k not in set(cancel_ids)}
        specs: list[TaskSpec] = []
        for spec in add_tasks:
            nid = local_to_new.get(spec.id, self.new_id()) if spec.id else self.new_id()
            rdeps: list[str] = []
            for d in spec.deps:
                if d in local_to_new:
                    rdeps.append(local_to_new[d])
                elif d in adj:
                    rdeps.append(d)
                else:
                    raise InvalidPlan(f"replan dep '{d}' is not a local alias or existing task id")
            clean_adj[nid] = rdeps
            specs.append(TaskSpec(id=nid, task=spec.task, agent=spec.agent,
                                  deps=rdeps, scope_paths=list(spec.scope_paths),
                                  cascade_policy=spec.cascade_policy))
        if _has_cycle(clean_adj):
            raise InvalidPlan("replan would create a cycle")
        if self.budget_state.tasks_used + len(specs) > self.budgets.max_tasks:
            raise BudgetExceeded("max_tasks would be exceeded by replan")
        await self.cancel_by_ids(cancel_ids, f"cancelled_by_replan_{replan_task_id}")
        for cid in cancel_ids:
            await self.cascade_cancel_recursive(cid)
        if specs:
            await self.insert_plan(specs, parent_id=target_parent_id,
                                   parent_depth=max(0, target_depth - 1),
                                   parent_root_id=target_root_id or None)
            self._charge_tasks(len(specs))
        await self.refresh_graph()
        return {"added": len(specs), "cancelled": len(cancel_ids)}

    async def compute_final_statuses(self) -> set[str]:
        return set((await self.get_statuses()).values())

    async def known_task_ids(self) -> set[str]:
        return await self.get_task_ids()

    async def done_sibling_ids(self, *, task_id: str, parent_id: str | None, since: float | None = None) -> list[str]:
        return await self.get_done_sibling_ids(task_id=task_id, parent_id=parent_id, since=since)

    # ---- checkpoints -------------------------------------------------------

    async def checkpoint(self, label: str | None, project_context: Any) -> TeamRunCheckpoint:
        await self.refresh_graph()
        async with self.lock:
            self._checkpoint_seq += 1
            cp = TeamRunCheckpoint(
                id=str(uuid.uuid4()), team_run_id=self._team_run_id,
                sequence=self._checkpoint_seq, taken_at=_utcnow(), label=label,
                tasks=copy.deepcopy(self.graph),
                ready_queue_order=list(self._ready_order),
                project_context=copy.deepcopy(project_context),
                budget_state=copy.deepcopy(self.budget_state),
            )
            self._checkpoints.append(cp)
            if self._checkpoint_store is not None and getattr(self._checkpoint_store, "initialized", False):
                try:
                    await self._checkpoint_store.save(cp)
                except Exception:
                    logger.debug("Failed to persist checkpoint %s", cp.id, exc_info=True)
            self._emit(make_checkpoint_taken(self._team_run_id, checkpoint_id=cp.id, sequence=cp.sequence, label=label))
            return cp

    def list_checkpoints(self) -> list[TeamRunCheckpoint]:
        return list(self._checkpoints)

    def _get_checkpoint(self, checkpoint_id: str) -> TeamRunCheckpoint | None:
        return next((cp for cp in self._checkpoints if cp.id == checkpoint_id), None)

    async def _get_checkpoint_with_fallback(self, checkpoint_id: str) -> TeamRunCheckpoint | None:
        cp = self._get_checkpoint(checkpoint_id)
        if cp is not None:
            return cp
        if self._checkpoint_store is not None and getattr(self._checkpoint_store, "initialized", False):
            rec = await self._checkpoint_store.load_by_id(checkpoint_id, self._team_run_id)
            if rec is not None:
                return self._record_to_checkpoint(rec)
        return None

    @staticmethod
    def _record_to_checkpoint(rec: Any) -> TeamRunCheckpoint:
        from datetime import datetime
        tasks: dict[str, Task] = {}
        for tid, td in (rec.tasks or {}).items():
            for f in ("created_at", "started_at", "finished_at"):
                val = td.get(f)
                if isinstance(val, str) and val:
                    try:
                        td[f] = datetime.fromisoformat(val)
                    except ValueError:
                        td[f] = None
                elif not isinstance(val, datetime):
                    td[f] = None
            if "status" in td:
                td["status"] = TaskStatus(td["status"])
            tasks[tid] = Task(**td)
        return TeamRunCheckpoint(
            id=rec.id, team_run_id=rec.team_run_id, sequence=rec.sequence,
            taken_at=rec.taken_at, label=rec.label, tasks=tasks,
            ready_queue_order=list(rec.ready_queue_order or []),
            project_context=rec.project_context,
            budget_state=BudgetState(**(rec.budget_state or {})),
        )

    async def rollback_to(self, checkpoint_id: str, project_context_setter: Callable[[Any], None]) -> TeamRunCheckpoint:
        cp = await self._get_checkpoint_with_fallback(checkpoint_id)
        if cp is None:
            raise CheckpointNotFound(checkpoint_id)
        await self.replace_run_tasks(list(cp.tasks.values()))
        self.graph = copy.deepcopy(cp.tasks)
        self._ready_order = list(cp.ready_queue_order)
        self.budget_state = copy.deepcopy(cp.budget_state)
        project_context_setter(copy.deepcopy(cp.project_context))
        return cp

    async def prepare_for_resume(self) -> None:
        if self._resume_snapshot is not None:
            await self.replace_run_tasks(self._resume_snapshot)
            self._resume_snapshot = None
        recovered = await self.recover_running()
        if recovered:
            logger.info("Recovered %d running tasks to ready", len(recovered))
        await self.refresh_graph()
