"""TaskCenter — unified task lifecycle orchestration.

Composes specialized managers and exposes a single facade for the rest of
the runtime:

- ``TaskStore``       — persistence + in-memory task graph
- ``BudgetManager``   — task/replan capacity + budget_update events
- ``PlanExpander``    — submitted-plan validation, expansion, replan apply
- ``TransitionTracker`` — diff/emit task state-change events
- ``NoteManager``     — note posting, scope filtering
- ``TaskContextBuilder`` — agent prompt context assembly
- ``CheckpointManager`` — snapshot ring buffer + rollback
"""

from __future__ import annotations

import logging
import uuid
from typing import Any, Awaitable, Callable, TypeVar

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.budget_manager import BudgetManager
from team.checkpoint_manager import CheckpointManager
from team.errors import CheckpointNotFound, InvalidPlan
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Note,
    ReplanRequest,
    TERMINAL_STATUSES,
    Task,
    TaskDefinition,
    _utcnow,
)
from team.note_manager import NoteManager
from team.persistence.events import (
    TeamRunEvent,
    make_checkpoint_taken,
    make_replace_dependency,
    make_task_added,
    task_to_dict,
)
from team.persistence.run_store import NullTeamRunStore, TeamRunStore
from team.persistence.task_store import TaskStore, _has_parent_summarizer_role
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
        self._fail_fast_cb: Callable[[str], Awaitable[None]] | None = None
        self._notes: NoteManager

        self._notes = NoteManager(
            team_run_id=team_run_id,
            event_store_cb=self._emit,
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

    def set_fail_fast_callback(
        self, callback: Callable[[str], Awaitable[None]] | None
    ) -> None:
        self._fail_fast_cb = callback

    def _cancel_running_task(self, task_id: str) -> None:
        if self._cancel_running_task_cb is not None:
            self._cancel_running_task_cb(task_id)

    async def _handle_awaiting_summary_ids(self, awaiting: list[str]) -> None:
        for parent_id in awaiting:
            parent = self.graph.get(parent_id)
            if parent is not None:
                self._transitions.emit_full_status(parent)
            await self._ensure_parent_summary_task(parent_id)

    def _resolve_parent_summarizer_agent_name(self) -> str:
        """Resolve the parent-summarizer agent name from team roster, then registry.

        Only accepts roster entries whose registered definition carries
        ``role == "parent_summarizer"``. A roster entry pointing at a
        non-summarizer definition is ignored so the completion-side role guard
        (``_has_parent_summarizer_role``) cannot silently fail and wedge the
        parent in EAS.
        """
        default = "parent_summarizer"
        try:
            from team.runtime.registry import get as get_team_run

            team_run = get_team_run(self._team_run_id)
        except Exception:
            team_run = None
        if team_run is not None:
            roster = getattr(team_run, "roster", None)
            if isinstance(roster, dict):
                candidates = roster.get("parent_summarizer")
                if isinstance(candidates, list):
                    from agents.registry import get_definition

                    for candidate in candidates:
                        name = str(candidate).strip()
                        if not name:
                            continue
                        defn = get_definition(name)
                        if defn is None:
                            continue
                        if getattr(defn, "role", None) != "parent_summarizer":
                            logger.warning(
                                "roster entry %r for parent_summarizer has "
                                "role=%r; ignoring",
                                name,
                                getattr(defn, "role", None),
                            )
                            continue
                        return name
        return default

    def _live_parent_summary_task_from_graph(self, parent_id: str) -> Task | None:
        for task in self.graph.values():
            if task.fired_by_task_id != parent_id:
                continue
            if task.status in TERMINAL_STATUSES:
                continue
            if _has_parent_summarizer_role(task.agent_name):
                return task
        return None

    async def _ensure_parent_summary_task(self, parent_id: str) -> None:
        """Inject a dispatchable parent-summary task for an EAS parent.

        The task is a direct child of the EAS parent with
        ``fired_by_task_id = parent_id``. The normal Executor path then runs
        the summarizer as a first-class team task; when it completes,
        ``complete_task`` posts the authoritative parent-summary note and
        finalizes the EAS parent to DONE.

        Idempotent against restart-recovery: if a ``parent_summary``-tagged
        note already exists for ``parent_id`` (from a previous lifetime that
        posted the note but crashed before ``finalize_parent_awaiting_summary``
        committed), skip spawning a new summarizer and finalize directly.
        """
        try:
            existing_notes = await self._notes.read(
                authors=[parent_id], tags=["parent_summary"]
            )
        except Exception:
            existing_notes = []
        if existing_notes:
            logger.info(
                "ensure_parent_summary_task: parent=%s already has a "
                "parent_summary note; finalizing without re-spawning",
                parent_id,
            )
            try:
                await self.finalize_parent_awaiting_summary(parent_id)
            except Exception:
                logger.exception(
                    "finalize_parent_awaiting_summary failed during recovery "
                    "for parent=%s",
                    parent_id,
                )
            return

        parent = self.graph.get(parent_id)
        if parent is None:
            await self._store.refresh_graph()
            parent = self.graph.get(parent_id)
            if parent is None:
                logger.warning(
                    "ensure_parent_summary_task: parent %s missing from graph",
                    parent_id,
                )
                return
        existing_summary_task = self._live_parent_summary_task_from_graph(parent_id)
        if existing_summary_task is not None:
            self._transitions.emit_full_status(existing_summary_task)
            return
        children = [
            task for task in self.graph.values()
            if getattr(task, "parent_id", None) == parent_id
        ]
        from prompt.external_trigger_prompts import build_parent_summary_prompt

        objective = build_parent_summary_prompt(parent, children)
        summarizer_agent = self._resolve_parent_summarizer_agent_name()
        summary_task, created = await self._store.insert_parent_summary_task(
            parent_task=parent,
            summarizer_agent=summarizer_agent,
            objective=objective,
        )
        if created:
            self._emit(make_task_added(self._team_run_id, task_to_dict(summary_task)))
        self._transitions.emit_full_status(summary_task)

    async def _post_parent_summary_note(
        self, summary_task: Task, summary_content: str
    ) -> bool:
        """Post the authoritative parent-summary note against the EAS parent.

        Tagged ``["implementation", "parent_summary"]`` so downstream readers
        (grandparent summarizer, dependents, humans) find it under the parent
        task's id, not the ephemeral summarizer's id.
        """
        parent_id = summary_task.fired_by_task_id
        if not parent_id:
            return False
        try:
            existing = await self._notes.read(
                authors=[parent_id], tags=["parent_summary"]
            )
        except Exception:
            existing = []
        if existing:
            logger.info(
                "parent_summary note already present for parent=%s; skipping "
                "duplicate post (summary_task=%s)",
                parent_id,
                summary_task.id,
            )
            return True
        parent = self.graph.get(parent_id)
        scope_paths = list(getattr(parent, "scope_paths", []) or [])
        try:
            await self._notes.post(
                Note(
                    id=self._new_id(),
                    task_id=parent_id,
                    agent_name=summary_task.agent_name,
                    content=summary_content,
                    paths=scope_paths,
                    tags=["implementation", "parent_summary"],
                )
            )
        except Exception:
            logger.exception(
                "parent_summary note post failed for parent=%s summary_task=%s",
                parent_id,
                summary_task.id,
            )
            return False
        return True

    async def _emit_replanned_origin_if_finalized(self, replanner_task_id: str) -> None:
        origin_id = await self._store.finalize_replanned_origin(replanner_task_id)
        if origin_id is None:
            return
        origin = self.graph.get(origin_id)
        if origin is not None:
            self._transitions.emit_full_status(origin)
        promoted, awaiting = await self._store.maybe_promote_expanded_parent(origin_id)
        for promoted_id in promoted:
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is None:
                continue
            self._transitions.emit_full_status(promoted_task)
            if promoted_task.fired_by_task_id:
                await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._handle_awaiting_summary_ids(awaiting)

    async def _mark_done_emit_promotions(self, task_id: str) -> None:
        promoted_ready = await self._store.mark_done(task_id)
        self._transitions.emit_status(task_id, "done", finished_at=_utcnow().isoformat())
        for dep_id in promoted_ready:
            dep_task = self.graph.get(dep_id)
            if dep_task is None:
                continue
            self._transitions.emit_full_status(dep_task)
        promoted, awaiting = await self._store.maybe_promote_expanded_parent(task_id)
        for promoted_id in promoted:
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is None:
                continue
            self._transitions.emit_full_status(promoted_task)
            if promoted_task.fired_by_task_id:
                await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._handle_awaiting_summary_ids(awaiting)

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
                await self._emit_replanned_origin_if_finalized(task_id)
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
            await self._maybe_finalize_parent_via_summary_task(task_id, result)
        await self._store.refresh_graph()
        return new_items

    async def _maybe_finalize_parent_via_summary_task(
        self, summary_task_id: str, result: AgentResult
    ) -> None:
        """If ``summary_task_id`` is a parent_summarizer sidecar, finalize its parent.

        Parent-summary tasks carry ``fired_by_task_id = parent_id`` and run the
        ``parent_summarizer`` role. When they complete with a success summary,
        post the authoritative ``parent_summary`` note against the parent id
        and transition the EAS parent to DONE via the normal finalize cascade.
        """
        summary_task = self.graph.get(summary_task_id)
        if summary_task is None:
            return
        if not summary_task.fired_by_task_id:
            return
        if not _has_parent_summarizer_role(summary_task.agent_name):
            return
        summary_content = (result.summary or "").strip()
        if summary_content:
            posted = await self._post_parent_summary_note(summary_task, summary_content)
            if posted:
                await self.finalize_parent_awaiting_summary(summary_task.fired_by_task_id)
            else:
                await self.fail_parent_awaiting_summary(
                    summary_task.fired_by_task_id, "parent_summary_note_post_failed"
                )
        else:
            await self.fail_parent_awaiting_summary(
                summary_task.fired_by_task_id, "parent_summary_empty"
            )

    async def _fail_leaf(self, task_id: str, reason: str) -> None:
        """Mark a leaf task FAILED and emit transitions.

        Only leaf workers may fail; `TaskStore.fail_task` raises on EXPANDED.
        """
        before = self._transitions.snapshot()
        await self._store.fail_task(task_id, reason)
        await self._transitions.refresh_and_emit(before)

    async def fail_task(self, task_id: str, reason: str) -> None:
        # A (REQUEST_REPLAN) is already terminal. When its replanner R fails,
        # A stays at REQUEST_REPLAN; only R transitions to FAILED here, and
        # its cascade handles dependents that were rewired onto R.
        failing_task = self.graph.get(task_id)
        before = self._transitions.snapshot()
        await self._store.fail_task(task_id, reason)
        # FAILED children are now detached; parent may become promotable.
        promoted, awaiting = await self._store.maybe_promote_expanded_parent(task_id)
        for promoted_id in promoted:
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is not None:
                self._transitions.emit_full_status(promoted_task)
                if promoted_task.fired_by_task_id:
                    await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._handle_awaiting_summary_ids(awaiting)
        await self._transitions.refresh_and_emit(before)
        # Parent-summary sidecar failure must fail its EAS parent and
        # fail-fast the whole run — the parent cannot reach DONE without an
        # authoritative summary.
        if (
            failing_task is not None
            and failing_task.fired_by_task_id
            and _has_parent_summarizer_role(failing_task.agent_name)
        ):
            await self.fail_parent_awaiting_summary(
                failing_task.fired_by_task_id, "parent_summary_task_failed"
            )

    async def fail(self, task_id: str, reason: str) -> None:
        await self.fail_task(task_id, reason)

    async def force_fail_task(self, task_id: str, reason: str) -> None:
        """Mark a task FAILED for fatal runtime failures.

        This bypasses leaf-only failure rules used for normal agent outcomes.
        It is reserved for persistence/runtime exceptions where continuing the
        graph would leave tasks wedged in non-dispatchable states.
        """
        before = self._transitions.snapshot()
        await self._store.mark_terminal(task_id, "failed", reason)
        await self._transitions.refresh_and_emit(before)

    async def request_replan(self, task_id: str, request: ReplanRequest) -> Task:
        self._budget.require_replan_capacity()
        from agents.registry import find_by_role

        replanners = find_by_role("replanner")
        if not replanners:
            raise RuntimeError("no agent with role='replanner' is registered")
        before = self._transitions.snapshot()
        deps_before = {tid: list(task.deps) for tid, task in self.graph.items()}
        rec, is_new = await self._store.request_replan(
            task_id,
            reason=request.reason,
            suggestion=request.suggestion,
            replanner_agent=replanners[0].name,
        )
        task = self.graph[rec.id]
        if is_new:
            await self._transitions.refresh_and_emit(before)
            self._budget.bump_replan_counters()
            self._emit(make_task_added(self._team_run_id, task_to_dict(task)))
            rewired_task_ids = [
                tid
                for tid, old_deps in deps_before.items()
                if task_id in old_deps
                and rec.id in getattr(self.graph.get(tid), "deps", [])
            ]
            self._emit(
                make_replace_dependency(
                    self._team_run_id,
                    old_dep_id=task_id,
                    new_dep_ids=[rec.id],
                    task_ids=rewired_task_ids,
                )
            )
            self._budget.emit_update()
        else:
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
        except InvalidPlan:
            # Replan expansion failed. Origin A is already terminal at
            # REQUEST_REPLAN, so no origin-side transition is needed. The
            # replanner R itself is failed by the executor through the normal
            # worker-failure path.
            await self._transitions.refresh_and_emit(before)
            raise

        # Cancellations during replan may have detached every remaining child
        # of an EXPANDED parent — sweep to resolve promotions.
        promoted, awaiting = await self._store.sweep_expanded_promotions()
        for promoted_id in promoted:
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is not None:
                self._transitions.emit_full_status(promoted_task)
                if promoted_task.fired_by_task_id:
                    await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._handle_awaiting_summary_ids(awaiting)
        await self._transitions.refresh_and_emit(before)
        return outcome

    async def finalize_parent_awaiting_summary(self, parent_id: str) -> None:
        """Transition an EXPANDED_AWAITING_SUMMARY parent to DONE.

        Called after the parent-summary sidecar posts its roll-up. Runs the
        same dependent / origin / grandparent promotion cascade as a normal
        DONE transition.
        """
        rec = await self._store.get_record(parent_id)
        if rec is None:
            return
        if rec.status != "expanded_awaiting_summary":
            # Already finalized (idempotent) — nothing to do.
            return
        promoted_ready = await self._store.finalize_parent_summary(parent_id)
        self._transitions.emit_status(
            parent_id, "done", finished_at=_utcnow().isoformat()
        )
        for dep_id in promoted_ready:
            dep_task = self.graph.get(dep_id)
            if dep_task is not None:
                self._transitions.emit_full_status(dep_task)
        # Grandparent cascade: the now-DONE parent may satisfy its own
        # parent's promotion condition.
        promoted, awaiting = await self._store.maybe_promote_expanded_parent(parent_id)
        for promoted_id in promoted:
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is None:
                continue
            self._transitions.emit_full_status(promoted_task)
            if promoted_task.fired_by_task_id:
                await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._handle_awaiting_summary_ids(awaiting)
        # If the finalized parent is itself a replanner, resolve the origin.
        parent_task = self.graph.get(parent_id)
        if parent_task is not None and parent_task.fired_by_task_id:
            await self._emit_replanned_origin_if_finalized(parent_id)
        await self._store.refresh_graph()

    async def fail_parent_awaiting_summary(
        self, parent_id: str, reason: str
    ) -> None:
        """Transition an EXPANDED_AWAITING_SUMMARY parent to FAILED and fail-fast.

        Used when the parent-summary sidecar cannot produce an authoritative
        roll-up: empty summary, no terminal tool after retries, or leaf
        failure of the summarizer task itself. Mirrors the tail of
        ``finalize_parent_awaiting_summary`` but writes ``failed`` instead of
        ``done``, and escalates to the team-run ``fail_fast`` callback so the
        whole run terminates rather than leaving the parent wedged.
        """
        rec = await self._store.get_record(parent_id)
        if rec is None:
            return
        if rec.status != "expanded_awaiting_summary":
            return
        await self._store.mark_terminal(parent_id, "failed", reason)
        self._transitions.emit_status(
            parent_id, "failed", finished_at=_utcnow().isoformat()
        )
        promoted, awaiting = await self._store.maybe_promote_expanded_parent(parent_id)
        for promoted_id in promoted:
            promoted_task = self.graph.get(promoted_id)
            if promoted_task is None:
                continue
            self._transitions.emit_full_status(promoted_task)
            if promoted_task.fired_by_task_id:
                await self._emit_replanned_origin_if_finalized(promoted_id)
        await self._handle_awaiting_summary_ids(awaiting)
        await self._store.refresh_graph()
        if self._fail_fast_cb is not None:
            await self._fail_fast_cb(reason)

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
