"""TeamRun lifecycle container."""

from __future__ import annotations

import asyncio
import uuid
from typing import Any, Callable

from team.artifact_store import InMemoryArtifactStore
from team.context.project import ProjectContext
from team.dispatcher import Dispatcher
from team.types import (
    BudgetConfig,
    BudgetState,
    TeamDefinition,
    TeamRunStatus,
    WorkItem,
    WorkItemStatus,
)
from team.worker import Worker


class TeamRun:
    def __init__(
        self,
        *,
        session_id: str,
        user_request: str,
        budgets: BudgetConfig | None = None,
        goal: str | None = None,
        sandbox_id: str | None = None,
    ) -> None:
        self.id = str(uuid.uuid4())
        self.session_id = session_id
        self.user_request = user_request
        self.sandbox_id = sandbox_id
        self.budgets = budgets or BudgetConfig()
        self.budget_state = BudgetState()
        self.status = TeamRunStatus.PENDING
        self.project_context = ProjectContext(
            goal=goal or user_request, user_request=user_request
        )
        self.artifacts = InMemoryArtifactStore(self.budgets, self.budget_state)
        self.dispatcher = Dispatcher(
            team_run_id=self.id,
            budgets=self.budgets,
            budget_state=self.budget_state,
            artifact_store=self.artifacts,
        )
        self.cancel_event = asyncio.Event()
        self.root_work_item_id: str | None = None
        self._worker_tasks: list[asyncio.Task[None]] = []
        self._worker_factory: Callable[["TeamRun"], Worker] | None = None
        self._num_workers: int = 1

    # ---- lifecycle -------------------------------------------------------

    async def start(
        self,
        agent_name: str,
        payload: dict[str, Any],
        *,
        worker_factory: Callable[["TeamRun"], Worker],
        num_workers: int = 1,
    ) -> None:
        root = WorkItem(
            id=str(uuid.uuid4()),
            team_run_id=self.id,
            agent_name=agent_name,
            status=WorkItemStatus.PENDING,
            payload=dict(payload),
            depth=0,
        )
        root.root_id = root.id
        self.root_work_item_id = root.id
        await self.dispatcher.add_work_item(root)
        self.status = TeamRunStatus.RUNNING

        self._worker_factory = worker_factory
        self._num_workers = num_workers
        self._spawn_workers()

    async def start_with_team_definition(
        self,
        team_def: TeamDefinition,
        payload: dict[str, Any],
        *,
        worker_factory: Callable[["TeamRun"], Worker],
        num_workers: int = 1,
    ) -> None:
        """Start a team run using a ``TeamDefinition`` to pick the planner.

        Validates that ``team_def.planner_agent`` resolves in ``agents.registry``
        before dispatching the root WorkItem. Broken references fail fast
        with a descriptive error; the TeamRun stays in ``PENDING`` status
        and no workers are spawned.
        """
        # Lazy import — avoids a module-level dependency cycle between
        # ``team.run`` and ``agents.registry``.
        from agents.registry import get_definition

        if get_definition(team_def.planner_agent) is None:
            raise ValueError(
                f"team_definition '{team_def.name}' references planner agent "
                f"'{team_def.planner_agent}' which does not exist"
            )
        await self.start(
            agent_name=team_def.planner_agent,
            payload=payload,
            worker_factory=worker_factory,
            num_workers=num_workers,
        )

    def _spawn_workers(self) -> None:
        assert self._worker_factory is not None, "worker_factory not set"
        for _ in range(self._num_workers):
            worker = self._worker_factory(self)
            self._worker_tasks.append(asyncio.create_task(worker.run_forever()))

    async def wait(self) -> TeamRunStatus:
        while not self.dispatcher.all_terminal():
            await asyncio.sleep(0.05)
        await self._join_workers()
        self._compute_final_status()
        return self.status

    async def _join_workers(self) -> None:
        """Cooperative shutdown after the DAG has reached a terminal state.

        Unlike ``_drain_workers``, this does NOT cancel running items —
        the graph is already terminal so no item should be RUNNING.
        """
        self.cancel_event.set()
        for t in self._worker_tasks:
            try:
                await asyncio.wait_for(t, timeout=2.0)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                t.cancel()
        self._worker_tasks = []
        self.cancel_event.clear()

    async def _drain_workers(self) -> None:
        """Forceful drain used by rollback/cancel — kills any RUNNING item."""
        self.cancel_event.set()
        for t in self._worker_tasks:
            try:
                await asyncio.wait_for(t, timeout=2.0)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                t.cancel()
        self._worker_tasks = []
        await self.dispatcher.cancel_running("drained by rollback/cancel")
        self.cancel_event.clear()

    def _compute_final_status(self) -> None:
        statuses = {wi.status for wi in self.dispatcher.graph.values()}
        if WorkItemStatus.FAILED in statuses:
            self.status = TeamRunStatus.FAILED
        elif WorkItemStatus.CANCELLED in statuses:
            self.status = TeamRunStatus.CANCELLED
        else:
            self.status = TeamRunStatus.SUCCEEDED

    async def cancel(self) -> None:
        self.cancel_event.set()
        await self.dispatcher.cancel_all_pending()

    # ---- checkpoint API --------------------------------------------------

    async def checkpoint(self, label: str | None = None) -> str:
        cp = await self.dispatcher.checkpoint(
            label=label,
            project_context=self.project_context,
        )
        return cp.id

    async def rollback_to(self, checkpoint_id: str) -> None:
        # Phase 1 — cooperative drain.
        self.cancel_event.set()
        await self._drain_workers()
        # Phase 2 — atomic restore.
        await self.dispatcher.rollback_to(
            checkpoint_id,
            project_context_setter=lambda pc: setattr(self, "project_context", pc),
        )
        self.cancel_event.clear()
        # Phase 3 — respawn workers so the restored DAG actually drains.
        if self._worker_factory is not None:
            self._spawn_workers()
