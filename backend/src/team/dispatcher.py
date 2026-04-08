"""Dispatcher — DAG + ready queue + atomic mutations for a single TeamRun."""

from __future__ import annotations

import asyncio
import copy
import logging
import uuid
from datetime import datetime
from typing import TYPE_CHECKING, Any

from team.checkpoint import CheckpointStore, TeamRunCheckpoint, build_checkpoint
from team.types import (
    AgentResult,
    ArtifactTooLarge,
    BudgetConfig,
    BudgetState,
    CheckpointNotFound,
    InvalidPlan,
    WorkItem,
    WorkItemStatus,
    TERMINAL_WI_STATUSES,
)
from team.validation import validate_plan_phase_b

if TYPE_CHECKING:
    from team.artifact_store import InMemoryArtifactStore

logger = logging.getLogger(__name__)


class BudgetExceeded(Exception):
    pass


class Dispatcher:
    """Owns the WorkItem DAG for one TeamRun.

    All mutations happen under ``self.lock`` so concurrent Workers observe a
    consistent graph. Readiness is recomputed in place; the ready queue
    mirrors the exact set of WorkItems whose status is READY.
    """

    def __init__(
        self,
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        artifact_store: "InMemoryArtifactStore",
        checkpoint_store: CheckpointStore | None = None,
    ) -> None:
        self.team_run_id = team_run_id
        self.budgets = budgets
        self.budget_state = budget_state
        self.artifact_store = artifact_store
        self.checkpoint_store = checkpoint_store or CheckpointStore()
        self.graph: dict[str, WorkItem] = {}
        self._ready_queue: asyncio.Queue[str] = asyncio.Queue()
        self._ready_order: list[str] = []  # mirror of queue contents for checkpoints
        self.lock = asyncio.Lock()

    # ---- construction helpers --------------------------------------------

    def new_id(self) -> str:
        return str(uuid.uuid4())

    def _compute_readiness(self, wi: WorkItem) -> bool:
        """A WorkItem is READY iff all deps are DONE. Never reads parent_id."""
        if wi.status not in (WorkItemStatus.PENDING, WorkItemStatus.READY):
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
        """Insert a new WorkItem, enforcing budget caps, and enqueue if ready."""
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
        """Block until a ready WorkItem becomes available, then return its id."""
        wi_id = await self._ready_queue.get()
        # Remove first matching entry from ready_order mirror.
        try:
            self._ready_order.remove(wi_id)
        except ValueError:
            pass
        return wi_id

    async def mark_running(self, wi_id: str, agent_run_id: str) -> WorkItem:
        """Atomic READY→RUNNING with agent_run_id stamp."""
        async with self.lock:
            wi = self.graph[wi_id]
            if wi.status != WorkItemStatus.READY:
                raise RuntimeError(
                    f"mark_running: {wi_id} is {wi.status.value}, not READY"
                )
            wi.status = WorkItemStatus.RUNNING
            wi.agent_run_id = agent_run_id
            wi.started_at = datetime.utcnow()
            return wi

    async def complete(self, wi_id: str, result: AgentResult) -> list[WorkItem]:
        """Mark DONE and atomically insert any submitted Plan. Returns new items inserted."""
        new_items: list[WorkItem] = []
        async with self.lock:
            wi = self.graph[wi_id]
            if wi.status != WorkItemStatus.RUNNING:
                raise RuntimeError(
                    f"complete: {wi_id} is {wi.status.value}, not RUNNING"
                )

            # Phase B validation BEFORE storing the artifact — nothing partial lands.
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
                    wi.finished_at = datetime.utcnow()
                    wi.failure_reason = f"InvalidPlan: {e}"
                    self._cascade_cancel(wi_id)
                    return []
                # Enforce count budget before inserting
                if (
                    self.budget_state.work_items_used + len(new_items)
                    > self.budgets.max_work_items
                ):
                    wi.status = WorkItemStatus.FAILED
                    wi.finished_at = datetime.utcnow()
                    wi.failure_reason = "BudgetExceeded: max_work_items"
                    self._cascade_cancel(wi_id)
                    return []

            # Store artifact
            try:
                self.artifact_store.save(wi_id, result.artifact)
                wi.artifact_ref = wi_id
            except ArtifactTooLarge as e:
                wi.status = WorkItemStatus.FAILED
                wi.finished_at = datetime.utcnow()
                wi.failure_reason = f"ArtifactTooLarge: {e}"
                self._cascade_cancel(wi_id)
                return []

            # Insert new items atomically
            for nwi in new_items:
                self.graph[nwi.id] = nwi
                self.budget_state.work_items_used += 1

            # Mark parent DONE now that everything is persisted
            wi.status = WorkItemStatus.DONE
            wi.finished_at = datetime.utcnow()

            # Recompute readiness for new items and for existing successors of wi
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
            if wi is None:
                return
            if wi.status in TERMINAL_WI_STATUSES:
                return
            wi.status = WorkItemStatus.FAILED
            wi.finished_at = datetime.utcnow()
            wi.failure_reason = reason
            self._cascade_cancel(wi_id)

    def _cascade_cancel(self, wi_id: str) -> None:
        """Cancel everything transitively dependent on wi_id (forward dep walk)."""
        stack = [wi_id]
        seen: set[str] = set()
        while stack:
            cur = stack.pop()
            for other in self.graph.values():
                if cur in other.deps and other.id not in seen:
                    seen.add(other.id)
                    if other.status not in TERMINAL_WI_STATUSES:
                        other.status = WorkItemStatus.CANCELLED
                        other.finished_at = datetime.utcnow()
                        other.failure_reason = f"cascaded from {wi_id}"
                    stack.append(other.id)

    async def cancel_all_pending(self) -> None:
        """Mark every PENDING/READY WorkItem as CANCELLED. Used by TeamRun.cancel."""
        async with self.lock:
            for wi in self.graph.values():
                if wi.status in (WorkItemStatus.PENDING, WorkItemStatus.READY):
                    wi.status = WorkItemStatus.CANCELLED
                    wi.finished_at = datetime.utcnow()
                    wi.failure_reason = "team_run cancelled"

    async def cancel_descendants(self, wi_id: str) -> None:
        async with self.lock:
            self._cascade_cancel(wi_id)

    # ---- introspection ---------------------------------------------------

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
        change_log_entries: list[Any],
    ) -> TeamRunCheckpoint:
        async with self.lock:
            cp = build_checkpoint(
                team_run_id=self.team_run_id,
                label=label,
                store=self.checkpoint_store,
                work_items=self.graph,
                ready_queue_order=self._ready_order,
                artifacts=self.artifact_store.snapshot(),
                project_context=project_context,
                change_log_entries=change_log_entries,
                budget_state=self.budget_state,
            )
            self.checkpoint_store.save(cp)
            return cp

    async def rollback_to(
        self,
        checkpoint_id: str,
        project_context_setter,
        change_log_setter,
    ) -> TeamRunCheckpoint:
        """Atomically restore graph + artifacts + context from a checkpoint.

        Caller is responsible for cooperative drain (setting cancel_event,
        waiting for Workers to settle) BEFORE invoking this method.
        """
        async with self.lock:
            cp = self.checkpoint_store.get(checkpoint_id)
            if cp is None:
                raise CheckpointNotFound(checkpoint_id)

            self.graph = copy.deepcopy(cp.work_items)
            self.artifact_store.restore(cp.artifacts)
            self.budget_state.work_items_used = cp.budget_state.work_items_used
            self.budget_state.artifact_bytes_used = cp.budget_state.artifact_bytes_used
            project_context_setter(copy.deepcopy(cp.project_context))
            change_log_setter(copy.deepcopy(cp.change_log_entries))

            # Rebuild ready queue
            self._ready_queue = asyncio.Queue()
            self._ready_order = []
            for wi_id in cp.ready_queue_order:
                wi = self.graph.get(wi_id)
                if wi is not None and wi.status == WorkItemStatus.READY:
                    self._ready_queue.put_nowait(wi_id)
                    self._ready_order.append(wi_id)
            return cp

    async def delete_checkpoint(self, checkpoint_id: str) -> bool:
        async with self.lock:
            return self.checkpoint_store.delete(checkpoint_id)
