"""TaskCenter — unified task lifecycle orchestration.

Composes specialized managers and exposes a single facade for the rest of
the runtime:

- ``TaskStore``       — persistence + in-memory task graph
- ``BudgetManager``   — task/replan capacity + budget_update events
- ``PlanExpander``    — submitted-plan validation, expansion, replan apply
- ``TransitionTracker`` — diff/emit task state-change events
- ``NoteManager``     — note posting, scope filtering
- ``TaskContextBuilder`` — agent prompt context assembly
- ``ActivityTracker`` — edit/turn counters, auto note triggers
- ``CheckpointManager`` — snapshot ring buffer + rollback
"""

from __future__ import annotations

import logging
import uuid
from typing import Any, Awaitable, Callable, TypeVar

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.activity_tracker import ActivityTracker
from team.budget_manager import BudgetManager
from team.checkpoint_manager import CheckpointManager
from team.errors import CheckpointNotFound, InvalidPlan
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Note,
    ReplanRequest,
    Task,
    TaskDefinition,
    _utcnow,
)
from team.note_manager import NoteManager
from team.persistence.events import (
    TeamRunEvent,
    make_checkpoint_taken,
    make_task_added,
    task_to_dict,
)
from team.persistence.run_store import NullTeamRunStore, TeamRunStore
from team.persistence.task_store import TaskStore
from team.planning.expander import PlanExpander, ReplanApplyOutcome
from team.runtime.checkpoint import TeamRunCheckpoint
from team.runtime.transitions import TransitionTracker
from team.task_context_builder import TaskContextBuilder

logger = logging.getLogger(__name__)
_T = TypeVar("_T")


class TaskCenter:
    """Facade that orchestrates task lifecycle across specialized managers."""

    def __init__(
        self,
        *,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        arbiter: Any = None,
        max_checkpoints: int = 10,
        event_store: TeamRunStore | None = None,
    ) -> None:
        self._team_run_id = team_run_id
        self._store = TaskStore(session_factory, team_run_id)
        self._events: TeamRunStore = event_store or NullTeamRunStore()
        self._resume_snapshot: list[Task] | None = None

        self._budget = BudgetManager(
            team_run_id=team_run_id,
            budgets=budgets,
            budget_state=budget_state,
            emit_cb=self._emit,
        )

        self._transitions = TransitionTracker(
            team_run_id=team_run_id,
            graph_getter=lambda: self._store.graph,
            refresh_graph_fn=self._store.refresh_graph,
            emit_cb=self._emit,
        )

        self._expander = PlanExpander(
            team_run_id=team_run_id,
            store=self._store,
            budget=self._budget,
            graph_getter=lambda: self._store.graph,
            emit_cb=self._emit,
            fail_cb=self._fail_leaf,
            cancel_running_task_cb=self._cancel_running_task,
        )
        self._cancel_running_task_cb: Callable[[str], None] | None = None
        self._activity: ActivityTracker
        self._notes: NoteManager

        def _on_note_posted(note: Note) -> None:
            self._activity.on_note_posted(note)

        self._activity = ActivityTracker(
            team_run_id=team_run_id,
            graph_getter=lambda: self._store.graph,
            post_note_cb=lambda note: self._notes.post(note),
        )

        self._notes = NoteManager(
            team_run_id=team_run_id,
            event_store_cb=self._emit,
            note_posted_cb=_on_note_posted,
            get_task_fn=lambda tid: self.get_task(tid),
            task_store=self._store,
        )
        self._context = TaskContextBuilder(
            team_run_id=team_run_id,
            notes=self._notes,
            get_task_fn=lambda tid: self.get_task(tid),
            task_store=self._store,
            arbiter=arbiter,
        )

        self._checkpoints = CheckpointManager(
            team_run_id=team_run_id,
            max_checkpoints=max_checkpoints,
        )

    # ---- manager access (public) ----------------------------------------

    @property
    def store(self) -> TaskStore:
        return self._store

    @property
    def notes(self) -> NoteManager:
        return self._notes

    @property
    def context(self) -> TaskContextBuilder:
        return self._context

    @property
    def activity(self) -> ActivityTracker:
        return self._activity

    @property
    def checkpoints(self) -> CheckpointManager:
        return self._checkpoints

    @property
    def budget(self) -> BudgetManager:
        return self._budget

    # ---- budget attribute aliases ---------------------------------------

    @property
    def budgets(self) -> BudgetConfig:
        return self._budget.budgets

    @property
    def budget_state(self) -> BudgetState:
        return self._budget.budget_state

    @budget_state.setter
    def budget_state(self, value: BudgetState) -> None:
        self._budget.budget_state = value

    # ---- graph access ----------------------------------------------------

    @property
    def graph(self) -> dict[str, Task]:
        return self._store.graph

    @property
    def ready_queue_order(self) -> list[str]:
        return self._store.ready_queue_order

    def prime_resume_state(
        self,
        *,
        snapshot: list[Task],
        ready_queue_order: list[str],
    ) -> None:
        """Seed resume-only state rebuilt from an event log replay."""
        self._resume_snapshot = list(snapshot)
        self._store.ready_queue_order = ready_queue_order

    async def get_task(self, task_id: str) -> Task | None:
        return self._store.get_task(task_id)

    # ---- event emission --------------------------------------------------

    def _emit(self, event: TeamRunEvent) -> None:
        try:
            self._events.append(event)
        except Exception:
            logger.exception("team event store append failed; continuing")

    # ---- internal helpers ------------------------------------------------

    @staticmethod
    def _new_id() -> str:
        return str(uuid.uuid4())

    def set_cancel_running_task_callback(self, callback: Callable[[str], None] | None) -> None:
        self._cancel_running_task_cb = callback

    def _cancel_running_task(self, task_id: str) -> None:
        if self._cancel_running_task_cb is not None:
            self._cancel_running_task_cb(task_id)

    async def _emit_replanned_origin_if_finalized(self, replanner_task_id: str) -> None:
        origin_id = await self._store.finalize_replanned_origin(replanner_task_id)
        if origin_id is None:
            return
        origin = self.graph.get(origin_id)
        if origin is not None:
            self._transitions.emit_full_status(origin)
        for promoted_id in await self._store.maybe_promote_expanded_parent(origin_id):
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is None:
                continue
            self._transitions.emit_full_status(promoted_task)
            if promoted_task.fired_by_task_id:
                await self._emit_replanned_origin_if_finalized(promoted_id)

    async def _mark_done_emit_promotions(self, task_id: str) -> None:
        promoted_ready = await self._store.mark_done(task_id)
        self._transitions.emit_status(task_id, "done", finished_at=_utcnow().isoformat())
        for dep_id in promoted_ready:
            dep_task = self.graph.get(dep_id)
            if dep_task is None:
                continue
            self._transitions.emit_full_status(dep_task)
        for promoted_id in await self._store.maybe_promote_expanded_parent(task_id):
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is None:
                continue
            self._transitions.emit_full_status(promoted_task)
            if promoted_task.fired_by_task_id:
                await self._emit_replanned_origin_if_finalized(promoted_id)

    async def _with_transitions(
        self,
        op: Callable[[], Awaitable[_T]],
        *,
        filter_ids: set[str] | None = None,
    ) -> _T:
        """Snapshot → run op → refresh+emit transitions when op reports change."""
        before = self._transitions.snapshot(filter_ids)
        result = await op()
        if result:
            await self._transitions.refresh_and_emit(before)
        return result

    # ---- task lifecycle --------------------------------------------------

    async def add_task(self, t: Task) -> None:
        self._budget.require_capacity_for(1)
        await self._store.insert_plan(
            [
                TaskDefinition(
                    id=t.id,
                    objective=t.objective,
                    agent=t.agent_name,
                    description=t.description or "",
                    deps=list(t.deps),
                    scope_paths=list(t.scope_paths),
                )
            ],
            parent_id=t.parent_id,
            parent_depth=max(0, t.depth - 1) if t.parent_id else 0,
            parent_root_id=t.root_id or None,
        )
        self._budget.add_tasks_used(1)
        self._emit(make_task_added(self._team_run_id, task_to_dict(t)))
        self._budget.emit_update()

    async def complete_task(self, task_id: str, result: AgentResult) -> list[Task]:
        rec = await self._store.get_record(task_id)
        if rec is None or rec.status != "running":
            raise RuntimeError(
                f"complete: {task_id} is {rec.status if rec else 'missing'}, not RUNNING"
            )

        expansion = await self._expander.expand_submitted_plan(rec, result)
        if not expansion.accepted:
            return []
        new_items = list(expansion.new_items)

        if result.submitted_replan is not None:
            outcome = await self.apply_replan(
                replan_task_id=task_id,
                add_tasks=result.submitted_replan.add_tasks,
                cancel_ids=result.submitted_replan.cancel_ids,
            )
            if outcome.replanner_child_count > 0:
                await self._store.mark_expanded(task_id)
                self._transitions.emit_status(
                    task_id,
                    "expanded",
                    finished_at=_utcnow().isoformat(),
                )
            else:
                await self._mark_done_emit_promotions(task_id)
                await self._emit_replanned_origin_if_finalized(task_id)
            await self._store.refresh_graph()
            return new_items

        if result.submitted_plan is not None:
            await self._store.mark_expanded(task_id)
            self._transitions.emit_status(task_id, "expanded", finished_at=_utcnow().isoformat())
        else:
            await self._mark_done_emit_promotions(task_id)
        await self._store.refresh_graph()
        return new_items

    async def _fail_leaf(self, task_id: str, reason: str) -> None:
        """Mark a leaf task FAILED and emit transitions.

        Only leaf workers may fail; `TaskStore.fail_task` raises on EXPANDED.
        """
        before = self._transitions.snapshot()
        await self._store.fail_task(task_id, reason)
        await self._transitions.refresh_and_emit(before)

    async def fail_task(self, task_id: str, reason: str) -> None:
        # If a replanner fails, also fail the original task it was fired for.
        # A is a leaf worker (REQUEST_REPLAN), so plain fail_task suffices.
        rec = await self._store.get_record(task_id)
        if rec and rec.fired_by_task_id:
            origin = await self._store.get_record(rec.fired_by_task_id)
            if origin and origin.status == "request_replan":
                await self._store.fail_task(
                    rec.fired_by_task_id, f"replanner_failed: {reason}"
                )
        before = self._transitions.snapshot()
        await self._store.fail_task(task_id, reason)
        # FAILED children are now detached; parent may become promotable.
        for promoted_id in await self._store.maybe_promote_expanded_parent(task_id):
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is not None:
                self._transitions.emit_full_status(promoted_task)
                if promoted_task.fired_by_task_id:
                    await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._transitions.refresh_and_emit(before)

    async def fail(self, task_id: str, reason: str) -> None:
        await self.fail_task(task_id, reason)

    async def request_replan(self, task_id: str, request: ReplanRequest) -> Task:
        self._budget.require_replan_capacity()
        from agents.registry import find_by_role

        replanners = find_by_role("replanner")
        if not replanners:
            raise RuntimeError("no agent with role='replanner' is registered")
        before = self._transitions.snapshot()
        rec, is_new = await self._store.request_replan(
            task_id,
            reason=request.reason,
            suggestion=request.suggestion,
            replanner_agent=replanners[0].name,
        )
        task = self.graph[rec.id]
        if is_new:
            self._budget.bump_replan_counters()
            self._emit(make_task_added(self._team_run_id, task_to_dict(task)))
            self._budget.emit_update()
        await self._transitions.refresh_and_emit(before)
        return task

    async def apply_replan(
        self,
        replan_task_id: str,
        add_tasks: list[TaskDefinition],
        cancel_ids: list[str],
    ) -> ReplanApplyOutcome:
        before = self._transitions.snapshot()
        try:
            outcome = await self._expander.apply_replan(
                replan_task_id=replan_task_id,
                add_tasks=add_tasks,
                cancel_ids=cancel_ids,
            )
        except InvalidPlan as exc:
            # Replan expansion failed — fail the origin (REQUEST_REPLAN leaf)
            # so it doesn't stay stuck in a non-terminal state forever.
            replanner_rec = await self._store.get_record(replan_task_id)
            if replanner_rec and replanner_rec.fired_by_task_id:
                await self._store.fail_task(
                    replanner_rec.fired_by_task_id,
                    f"replan_apply_failed: {exc}",
                )
            await self._transitions.refresh_and_emit(before)
            raise

        # Cancellations during replan may have detached every remaining child
        # of an EXPANDED parent — sweep to resolve promotions.
        for promoted_id in await self._store.sweep_expanded_promotions():
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is not None:
                self._transitions.emit_full_status(promoted_task)
                if promoted_task.fired_by_task_id:
                    await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._transitions.refresh_and_emit(before)
        return outcome

    async def fail_orphaned_replanning(self) -> int:
        """Force-fail tasks stuck in REQUEST_REPLAN with no live replanner."""
        before = self._transitions.snapshot()
        count = await self._store.fail_orphaned_replanning()
        if count:
            await self._transitions.refresh_and_emit(before)
        return count

    async def cancel_all_pending(self) -> int:
        return await self._with_transitions(self._store.cancel_all_pending)

    async def cancel_all_running(self, reason: str) -> int:
        return await self._with_transitions(lambda: self._store.cancel_all_running(reason))

    async def mark_running(self, task_id: str, agent_run_id: str) -> Task:
        rec = await self._store.mark_running(task_id, agent_run_id)
        if rec is None:
            raise RuntimeError(f"mark_running: {task_id} not found")
        task = self.graph[task_id]
        self._transitions.emit_status(
            task_id,
            "running",
            agent_run_id=agent_run_id,
            started_at=task.started_at.isoformat() if task.started_at else None,
        )
        return task

    # ---- checkpointing ---------------------------------------------------

    async def checkpoint(self, label: str | None, project_context: Any) -> TeamRunCheckpoint:
        await self._store.refresh_graph()
        return await self._checkpoints.checkpoint(
            label=label,
            project_context=project_context,
            tasks=self.graph,
            ready_queue_order=self.ready_queue_order,
            notes=self._notes.snapshot(),
            budget_state=self.budget_state,
            emit_checkpoint_cb=lambda run_id, cp_id, seq, lbl: self._emit(
                make_checkpoint_taken(run_id, checkpoint_id=cp_id, sequence=seq, label=lbl)
            ),
        )

    def list_checkpoints(self) -> list[TeamRunCheckpoint]:
        return self._checkpoints.list_checkpoints()

    async def rollback_to(
        self, checkpoint_id: str, project_context_setter: Any
    ) -> TeamRunCheckpoint:
        cp = await self._checkpoints.rollback_to(
            checkpoint_id=checkpoint_id,
            project_context_setter=project_context_setter,
            replace_run_tasks_fn=self._store.replace_run_tasks,
            notes_restore_fn=self._notes.restore,
            ready_queue_order_setter=lambda order: setattr(self._store, "ready_queue_order", order),
        )
        if cp is None:
            raise CheckpointNotFound(checkpoint_id)
        await self._store.refresh_graph()
        self._budget.budget_state = cp.budget_state
        return cp

    async def prepare_for_resume(self) -> None:
        await self._checkpoints.prepare_for_resume(
            resume_snapshot=self._resume_snapshot,
            recover_running_fn=self._store.recover_running,
            replace_run_tasks_fn=self._store.replace_run_tasks,
        )
        self._resume_snapshot = None
        await self._store.refresh_graph()
