"""Dispatcher — DAG, ready queue, and atomic mutations for one TeamRun."""

from __future__ import annotations

import asyncio
import copy
import logging
import uuid
from collections import deque
from typing import TYPE_CHECKING, Any, Callable

from team.errors import (
    ArtifactTooLarge,
    BudgetExceeded,
    CheckpointNotFound,
    InvalidPlan,
)
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    TERMINAL_WI_STATUSES,
    WorkItem,
    WorkItemKind,
    WorkItemStatus,
    _utcnow,
)
from team.planning.validation import validate_plan_phase_b
from team.runtime.checkpoint import TeamRunCheckpoint

if TYPE_CHECKING:
    from team.artifacts.store import InMemoryArtifactStore

logger = logging.getLogger(__name__)


class Dispatcher:
    """Owns the WorkItem DAG for one TeamRun. Mutations are lock-protected."""

    def __init__(
        self,
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        artifact_store: "InMemoryArtifactStore",
        max_checkpoints: int = 10,
    ) -> None:
        self.team_run_id = team_run_id
        self.budgets = budgets
        self.budget_state = budget_state
        self.artifact_store = artifact_store
        self.graph: dict[str, WorkItem] = {}
        self._ready_queue: asyncio.Queue[str] = asyncio.Queue()
        self._ready_order: list[str] = []
        self.lock = asyncio.Lock()
        self._checkpoints: deque[TeamRunCheckpoint] = deque(maxlen=max_checkpoints)
        self._checkpoint_seq = 0

    def new_id(self) -> str:
        return str(uuid.uuid4())

    def _compute_readiness(self, wi: WorkItem) -> bool:
        """A WorkItem becomes READY iff PENDING and all deps are DONE."""
        if wi.status != WorkItemStatus.PENDING:
            return False
        for dep_id in wi.deps:
            dep = self.graph.get(dep_id)
            if dep is None or dep.status != WorkItemStatus.DONE:
                return False
        return True

    def _enqueue(self, wi: WorkItem) -> None:
        wi.status = WorkItemStatus.READY
        self._ready_queue.put_nowait(wi.id)
        self._ready_order.append(wi.id)

    async def add_work_item(self, wi: WorkItem) -> None:
        async with self.lock:
            if self.budget_state.work_items_used >= self.budgets.max_work_items:
                raise BudgetExceeded(
                    f"max_work_items={self.budgets.max_work_items} reached"
                )
            if wi.id in self.graph:
                raise ValueError(f"WorkItem {wi.id} already exists")
            self.graph[wi.id] = wi
            self.budget_state.work_items_used += 1
            if self._compute_readiness(wi):
                self._enqueue(wi)

    async def pop_ready(self) -> str:
        while True:
            wi_id = await self._ready_queue.get()
            async with self.lock:
                try:
                    self._ready_order.remove(wi_id)
                except ValueError:
                    pass
                wi = self.graph.get(wi_id)
                if wi is None or wi.status != WorkItemStatus.READY:
                    continue
                return wi_id

    async def mark_running(self, wi_id: str, agent_run_id: str) -> WorkItem:
        async with self.lock:
            wi = self.graph[wi_id]
            if wi.status != WorkItemStatus.READY:
                raise RuntimeError(
                    f"mark_running: {wi_id} is {wi.status.value}, not READY"
                )
            wi.status = WorkItemStatus.RUNNING
            wi.agent_run_id = agent_run_id
            wi.started_at = _utcnow()
            return wi

    async def complete(self, wi_id: str, result: AgentResult) -> list[WorkItem]:
        """Mark DONE and atomically insert any submitted Plan."""
        new_items: list[WorkItem] = []
        async with self.lock:
            wi = self.graph[wi_id]
            if wi.status != WorkItemStatus.RUNNING:
                raise RuntimeError(
                    f"complete: {wi_id} is {wi.status.value}, not RUNNING"
                )

            if wi.kind == WorkItemKind.EXPANDABLE and result.submitted_plan is None:
                wi.status = WorkItemStatus.FAILED
                wi.finished_at = _utcnow()
                wi.failure_reason = "InvalidPlan: expandable work item did not submit a plan"
                self._cascade_cancel(wi_id)
                return []

            if result.submitted_plan is not None:
                try:
                    new_items = validate_plan_phase_b(
                        existing_graph=self.graph,
                        plan=result.submitted_plan,
                        team_run_id=self.team_run_id,
                        parent_wi=wi,
                        new_id_factory=self.new_id,
                        max_depth=self.budgets.max_depth,
                    )
                except InvalidPlan as e:
                    wi.status = WorkItemStatus.FAILED
                    wi.finished_at = _utcnow()
                    wi.failure_reason = f"InvalidPlan: {e}"
                    self._cascade_cancel(wi_id)
                    return []
                if (
                    self.budget_state.work_items_used + len(new_items)
                    > self.budgets.max_work_items
                ):
                    wi.status = WorkItemStatus.FAILED
                    wi.finished_at = _utcnow()
                    wi.failure_reason = "BudgetExceeded: max_work_items"
                    self._cascade_cancel(wi_id)
                    return []

            try:
                self.artifact_store.save(wi_id, result.artifact)
                wi.artifact_ref = wi_id
            except ArtifactTooLarge as e:
                wi.status = WorkItemStatus.FAILED
                wi.finished_at = _utcnow()
                wi.failure_reason = f"ArtifactTooLarge: {e}"
                self._cascade_cancel(wi_id)
                return []

            for nwi in new_items:
                self.graph[nwi.id] = nwi
                self.budget_state.work_items_used += 1

            wi.status = WorkItemStatus.DONE
            wi.finished_at = _utcnow()

            touched: list[WorkItem] = list(new_items)
            for other in self.graph.values():
                if wi_id in other.deps and other.status == WorkItemStatus.PENDING:
                    touched.append(other)
            for t in touched:
                if self._compute_readiness(t):
                    self._enqueue(t)

        return new_items

    async def fail(self, wi_id: str, reason: str) -> None:
        async with self.lock:
            wi = self.graph.get(wi_id)
            if wi is None or wi.status in TERMINAL_WI_STATUSES:
                return
            wi.status = WorkItemStatus.FAILED
            wi.finished_at = _utcnow()
            wi.failure_reason = reason
            self._cascade_cancel(wi_id)

    def _cascade_cancel(self, wi_id: str) -> None:
        """Cancel everything transitively dependent on wi_id."""
        stack = [wi_id]
        seen: set[str] = set()
        while stack:
            cur = stack.pop()
            for other in self.graph.values():
                if cur in other.deps and other.id not in seen:
                    seen.add(other.id)
                    if other.status not in TERMINAL_WI_STATUSES:
                        other.status = WorkItemStatus.CANCELLED
                        other.finished_at = _utcnow()
                        other.failure_reason = f"cascaded from {wi_id}"
                    stack.append(other.id)

    async def cancel_all_pending(self) -> None:
        async with self.lock:
            for wi in self.graph.values():
                if wi.status in (WorkItemStatus.PENDING, WorkItemStatus.READY):
                    wi.status = WorkItemStatus.CANCELLED
                    wi.finished_at = _utcnow()
                    wi.failure_reason = "team_run cancelled"

    async def cancel_running(self, reason: str) -> None:
        """Mark any RUNNING items as CANCELLED. Used after a cooperative drain."""
        async with self.lock:
            for wi in self.graph.values():
                if wi.status == WorkItemStatus.RUNNING:
                    wi.status = WorkItemStatus.CANCELLED
                    wi.finished_at = _utcnow()
                    wi.failure_reason = reason

    async def cancel_descendants(self, wi_id: str) -> None:
        async with self.lock:
            self._cascade_cancel(wi_id)

    def ready_items(self) -> list[WorkItem]:
        return [wi for wi in self.graph.values() if wi.status == WorkItemStatus.READY]

    def successors(self, wi_id: str) -> list[WorkItem]:
        return [wi for wi in self.graph.values() if wi_id in wi.deps]

    def all_terminal(self) -> bool:
        return all(wi.status in TERMINAL_WI_STATUSES for wi in self.graph.values())

    # ---- checkpoint / rollback -------------------------------------------

    async def checkpoint(
        self,
        label: str | None,
        project_context: Any,
    ) -> TeamRunCheckpoint:
        async with self.lock:
            self._checkpoint_seq += 1
            cp = TeamRunCheckpoint(
                id=str(uuid.uuid4()),
                team_run_id=self.team_run_id,
                sequence=self._checkpoint_seq,
                taken_at=_utcnow(),
                label=label,
                work_items=copy.deepcopy(self.graph),
                ready_queue_order=list(self._ready_order),
                artifacts=self.artifact_store.snapshot(),
                project_context=copy.deepcopy(project_context),
                budget_state=copy.deepcopy(self.budget_state),
            )
            self._checkpoints.append(cp)
            return cp

    def list_checkpoints(self) -> list[TeamRunCheckpoint]:
        return list(self._checkpoints)

    def _get_checkpoint(self, checkpoint_id: str) -> TeamRunCheckpoint | None:
        return next((cp for cp in self._checkpoints if cp.id == checkpoint_id), None)

    async def rollback_to(
        self,
        checkpoint_id: str,
        project_context_setter: Callable[[Any], None],
    ) -> TeamRunCheckpoint:
        """Atomically restore graph + artifacts + context. Caller must drain workers first."""
        async with self.lock:
            cp = self._get_checkpoint(checkpoint_id)
            if cp is None:
                raise CheckpointNotFound(checkpoint_id)

            self.graph = copy.deepcopy(cp.work_items)
            self.artifact_store.restore(cp.artifacts)
            self.budget_state.work_items_used = cp.budget_state.work_items_used
            self.budget_state.artifact_bytes_used = cp.budget_state.artifact_bytes_used
            project_context_setter(copy.deepcopy(cp.project_context))

            while not self._ready_queue.empty():
                self._ready_queue.get_nowait()
            self._ready_order = []
            for wi_id in cp.ready_queue_order:
                wi = self.graph.get(wi_id)
                if wi is not None and wi.status == WorkItemStatus.READY:
                    self._ready_queue.put_nowait(wi_id)
                    self._ready_order.append(wi_id)
            return cp

    async def delete_checkpoint(self, checkpoint_id: str) -> bool:
        async with self.lock:
            cp = self._get_checkpoint(checkpoint_id)
            if cp is None:
                return False
            self._checkpoints.remove(cp)
            return True
