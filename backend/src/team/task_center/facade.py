"""TaskCenter — manager composition facade.

All task lifecycle transitions are owned by ``TaskStatusHandler``. TaskCenter
now only composes the managers the runtime wires together (``store`` /
``notes`` / ``context`` / ``budget`` / ``expander``) plus ``add_task`` for
the initial root insertion and a couple of read-through helpers.

Bulk cancellation flows through ``task_center.store.cancel_all_pending`` /
``cancel_all_running`` directly; the atomic ``ready → running`` claim runs
through ``task_center.store.mark_running`` from the executor.
"""

from __future__ import annotations

import logging

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.core.models import (
    BudgetConfig,
    BudgetState,
    Task,
)
from team.persistence.events import (
    TeamRunEvent,
    make_task_added,
    task_to_dict,
)
from team.persistence.run_store import TeamRunStore
from team.persistence.task_store import TaskStore
from team.planning.expander import PlanExpander
from team.task_center.budget import BudgetManager
from team.task_center.prompts import TaskContextBuilder
from team.task_center.notes import NoteManager

logger = logging.getLogger(__name__)


class TaskCenter:
    """Composition facade over the runtime's specialized managers."""

    def __init__(
        self,
        *,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        event_store: TeamRunStore | None = None,
    ) -> None:
        self._team_run_id = team_run_id
        self._store = TaskStore(session_factory, team_run_id)
        self._events: TeamRunStore = event_store or TeamRunStore()

        self._budget = BudgetManager(
            team_run_id=team_run_id,
            budgets=budgets,
            budget_state=budget_state,
            emit_cb=self.emit_event,
        )
        self._expander = PlanExpander(
            team_run_id=team_run_id,
            store=self._store,
            budget=self._budget,
            graph_getter=lambda: self._store.graph,
            emit_cb=self.emit_event,
        )
        self._notes = NoteManager(
            team_run_id=team_run_id,
            event_store_cb=self.emit_event,
        )
        self._context = TaskContextBuilder(
            team_run_id=team_run_id,
            get_task_fn=lambda tid: self.get_task(tid),
            task_store=self._store,
        )

    # ---- manager access (public) ---------------------------------------

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
    def budget(self) -> BudgetManager:
        return self._budget

    @property
    def expander(self) -> PlanExpander:
        return self._expander

    # ---- budget attribute aliases --------------------------------------

    @property
    def budgets(self) -> BudgetConfig:
        return self._budget.budgets

    @property
    def budget_state(self) -> BudgetState:
        return self._budget.budget_state

    @budget_state.setter
    def budget_state(self, value: BudgetState) -> None:
        self._budget.budget_state = value

    # ---- graph access --------------------------------------------------

    @property
    def graph(self) -> dict[str, Task]:
        return self._store.graph

    @property
    def ready_queue_order(self) -> list[str]:
        return self._store.ready_queue_order

    async def get_task(self, task_id: str) -> Task | None:
        return self._store.get_task(task_id)

    # ---- event emission ------------------------------------------------

    def emit_event(self, event: TeamRunEvent) -> None:
        try:
            self._events.append(event)
        except Exception:
            logger.exception("team event store append failed; continuing")

    # ---- initial task insertion ----------------------------------------

    async def add_task(self, t: Task) -> None:
        """Insert ``t`` as a root task (or standalone) and emit task_added."""
        self._budget.require_capacity_for(1)
        await self._store.insert_plan(
            [t.definition],
            parent_id=t.parent_id,
            parent_depth=max(0, t.depth - 1) if t.parent_id else 0,
            parent_root_id=t.root_id or None,
        )
        self._budget.add_tasks_used(1)
        self.emit_event(make_task_added(self._team_run_id, task_to_dict(t)))
        self._budget.emit_update()
