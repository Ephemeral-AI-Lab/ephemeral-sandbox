"""Dispatcher for one TeamRun backed entirely by the durable task store."""

from __future__ import annotations

import asyncio
import copy
import logging
import uuid
from collections import deque
from typing import Any, Callable

from team.errors import BudgetExceeded, CheckpointNotFound, InvalidPlan
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
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
    make_work_item_added,
    make_work_item_status,
    work_item_to_dict,
)
from team.persistence.run_store import NullTeamRunStore, TeamRunStore
from team.planning.validation import validate_plan
from team.runtime.checkpoint import TeamRunCheckpoint
from team.runtime.dispatcher_store import DispatcherStore

_logger = logging.getLogger(__name__)


def _record_to_task(rec: Any) -> Task:
    """Convert a durable task record to the runtime Task dataclass."""
    return Task(
        id=rec.id,
        team_run_id=rec.team_run_id,
        agent_name=rec.agent_name,
        status=TaskStatus(rec.status),
        task=rec.task,
        deps=list(rec.deps) if rec.deps else [],
        scope_paths=list(rec.scope_paths) if rec.scope_paths else [],
        cascade_policy=rec.cascade_policy or "cancel",
        parent_id=rec.parent_id,
        root_id=rec.root_id or "",
        depth=rec.depth or 0,
        retry_count=rec.retry_count or 0,
        max_retries=rec.max_retries or 2,
        agent_run_id=rec.agent_run_id,
        created_at=rec.created_at or _utcnow(),
        started_at=rec.started_at,
        finished_at=rec.finished_at,
        failure_reason=rec.failure_reason,
    )


class Dispatcher:
    """Coordinates TeamRun task execution against the durable task store."""

    def __init__(
        self,
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        *,
        store: DispatcherStore,
        max_checkpoints: int = 10,
        event_store: TeamRunStore | None = None,
        checkpoint_store: Any = None,
    ) -> None:
        self.team_run_id = team_run_id
        self.budgets = budgets
        self.budget_state = budget_state
        self.store = store
        self.graph: dict[str, Task] = {}
        self._ready_order: list[str] = []
        self._resume_snapshot: list[Task] | None = None
        self.lock = asyncio.Lock()
        self._checkpoints: deque[TeamRunCheckpoint] = deque(maxlen=max_checkpoints)
        self._checkpoint_seq = 0
        self._events: TeamRunStore = event_store or NullTeamRunStore()
        self.task_center: Any = None
        self._checkpoint_store = checkpoint_store

    def _emit(self, event: TeamRunEvent) -> None:
        try:
            self._events.append(event)
        except Exception:
            _logger.exception("team event store append failed; continuing")

    def _emit_budget(self) -> None:
        self._emit(
            make_budget_update(
                self.team_run_id,
                tasks_used=self.budget_state.tasks_used,
                note_bytes_used=self.budget_state.note_bytes_used,
                replans_used=self.budget_state.replans_used,
            )
        )

    def _charge_tasks(self, n: int = 1) -> None:
        """Increment tasks_used by n and emit the budget event."""
        self.budget_state.tasks_used += n
        self._emit_budget()

    async def _mark_failed_and_cascade(self, wi_id: str, reason: str) -> None:
        """Mark a work item failed, cancel its dependants, and refresh the graph."""
        await self.store.mark_failed(wi_id, self.team_run_id, reason)
        await self.store.cascade_cancel_recursive(self.team_run_id, wi_id)
        await self.refresh_graph()

    def new_id(self) -> str:
        return str(uuid.uuid4())

    async def refresh_graph(self) -> dict[str, Task]:
        records = await self.store.get_all_tasks(self.team_run_id)
        self.graph = {record.id: _record_to_task(record) for record in records}
        self._ready_order = [record.id for record in records if record.status == "ready"]
        return self.graph

    async def get_task(self, task_id: str) -> Task | None:
        rec = await self.store.get_task(task_id, self.team_run_id)
        if rec is None:
            self.graph.pop(task_id, None)
            return None
        task = _record_to_task(rec)
        self.graph[task.id] = task
        return task

    async def add_work_item(self, wi: Task) -> None:
        if self.budget_state.tasks_used >= self.budgets.max_tasks:
            raise BudgetExceeded(f"max_tasks={self.budgets.max_tasks} reached")

        await self.store.insert_plan(
            self.team_run_id,
            [
                TaskSpec(
                    id=wi.id,
                    task=wi.task,
                    agent=wi.agent_name,
                    deps=list(wi.deps),
                    scope_paths=list(wi.scope_paths),
                    cascade_policy=wi.cascade_policy,
                )
            ],
            parent_id=wi.parent_id,
            parent_depth=max(0, wi.depth - 1) if wi.parent_id else 0,
            parent_root_id=wi.root_id or None,
        )
        self.budget_state.tasks_used += 1
        wi.status = TaskStatus.READY if not wi.deps else TaskStatus.PENDING
        self.graph[wi.id] = wi
        if wi.status == TaskStatus.READY and wi.id not in self._ready_order:
            self._ready_order.append(wi.id)
        self._emit(make_work_item_added(self.team_run_id, work_item_to_dict(wi)))
        self._emit_budget()

    async def pop_ready(self) -> str:
        while True:
            rec = await self.store.pop_ready(self.team_run_id)
            if rec is not None:
                task = _record_to_task(rec)
                self.graph[task.id] = task
                try:
                    self._ready_order.remove(task.id)
                except ValueError:
                    pass
                return task.id
            await asyncio.sleep(0.05)

    async def mark_running(self, wi_id: str, agent_run_id: str) -> Task:
        rec = await self.store.mark_running(self.team_run_id, wi_id, agent_run_id)
        if rec is None:
            raise RuntimeError(f"mark_running: {wi_id} not found")
        task = _record_to_task(rec)
        self.graph[task.id] = task
        self._emit(
            make_work_item_status(
                self.team_run_id,
                wi_id,
                "running",
                agent_run_id=agent_run_id,
                started_at=task.started_at.isoformat() if task.started_at else None,
            )
        )
        return task

    async def complete(self, wi_id: str, result: AgentResult) -> list[Task]:
        new_items: list[Task] = []
        rec = await self.store.get_task(wi_id, self.team_run_id)
        if rec is None or rec.status != "running":
            raise RuntimeError(f"complete: {wi_id} is {rec.status if rec else 'missing'}, not RUNNING")

        from agents.registry import has_role as _has_role

        if _has_role(rec.agent_name, "planner") and result.submitted_plan is None:
            await self._mark_failed_and_cascade(wi_id, "InvalidPlan: expandable work item did not submit a plan")
            return []

        if result.submitted_plan is not None:
            new_depth = (rec.depth or 0) + 1
            if new_depth > self.budgets.max_depth:
                await self._mark_failed_and_cascade(wi_id, f"InvalidPlan: plan would exceed max_depth={self.budgets.max_depth}")
                return []

            adj = await self.store.get_adjacency(self.team_run_id)
            allow_empty = bool(rec.root_id) and wi_id != (rec.root_id or wi_id)
            issues = validate_plan(
                result.submitted_plan,
                max_plan_size=self.budgets.max_plan_size,
                allow_empty=allow_empty,
                known_external_deps=set(adj.keys()),
            )
            if issues:
                await self._mark_failed_and_cascade(wi_id, "InvalidPlan: " + "; ".join(i["msg"] for i in issues))
                return []

            local_to_global: dict[str, str] = {
                spec.id: self.new_id()
                for spec in result.submitted_plan.tasks
                if spec.id
            }
            specs: list[TaskSpec] = []
            for spec in result.submitted_plan.tasks:
                new_id = local_to_global.get(spec.id) or self.new_id()
                resolved_deps = [local_to_global[d] if d in local_to_global else d for d in spec.deps]
                specs.append(
                    TaskSpec(
                        id=new_id,
                        task=spec.task,
                        agent=spec.agent,
                        deps=resolved_deps,
                        scope_paths=list(spec.scope_paths),
                        cascade_policy=spec.cascade_policy,
                    )
                )
                new_items.append(
                    Task(
                        id=new_id,
                        team_run_id=self.team_run_id,
                        agent_name=spec.agent,
                        status=TaskStatus.READY if not resolved_deps else TaskStatus.PENDING,
                        task=spec.task,
                        deps=resolved_deps,
                        scope_paths=list(spec.scope_paths),
                        cascade_policy=spec.cascade_policy,
                        parent_id=wi_id,
                        root_id=rec.root_id or wi_id,
                        depth=new_depth,
                    )
                )

            if self.budget_state.tasks_used + len(new_items) > self.budgets.max_tasks:
                await self._mark_failed_and_cascade(wi_id, "BudgetExceeded: max_tasks")
                return []

            await self.store.insert_plan(
                self.team_run_id,
                specs,
                parent_id=wi_id,
                parent_depth=rec.depth or 0,
                parent_root_id=rec.root_id or wi_id,
            )
            self.budget_state.tasks_used += len(new_items)
            for task in new_items:
                self._emit(make_work_item_added(self.team_run_id, work_item_to_dict(task)))
            self._emit_budget()

        if result.submitted_plan is not None:
            # Planner expanded — hold dependents until children finish
            await self.store.mark_expanded(wi_id, self.team_run_id)
            self._emit(
                make_work_item_status(
                    self.team_run_id,
                    wi_id,
                    "expanded",
                    finished_at=_utcnow().isoformat(),
                )
            )
        else:
            await self.store.mark_done(wi_id, self.team_run_id)
            self._emit(
                make_work_item_status(
                    self.team_run_id,
                    wi_id,
                    "done",
                    finished_at=_utcnow().isoformat(),
                )
            )
            # Check if completing this task promotes an expanded parent
            promoted = await self.store.maybe_promote_expanded_parent(
                wi_id, self.team_run_id
            )
            for pid in promoted:
                self._emit(
                    make_work_item_status(
                        self.team_run_id,
                        pid,
                        "done",
                        finished_at=_utcnow().isoformat(),
                    )
                )

        if result.submitted_replan is not None:
            await self.apply_replan(
                replan_task_id=wi_id,
                add_tasks=result.submitted_replan.add_tasks,
                cancel_ids=result.submitted_replan.cancel_ids,
                target_depth=rec.depth or 0,
                target_parent_id=rec.parent_id,
                target_root_id=rec.root_id or "",
            )

        await self.refresh_graph()
        return new_items

    async def fail(self, wi_id: str, reason: str) -> None:
        warnings = await self.store.fail_task(self.team_run_id, wi_id, reason)
        # Post warnings for 'continue' policy dependents via TaskCenter
        if warnings and self.task_center is not None:
            from team.models import Note
            for dep_task_id, warning_msg in warnings:
                note = Note(
                    task_id=dep_task_id,
                    agent_name="system",
                    content=warning_msg,
                )
                try:
                    await self.task_center.post(note)
                except Exception:
                    _logger.debug("Failed to post warning note for %s", dep_task_id, exc_info=True)
        await self.refresh_graph()

    async def retry_work_item(self, wi_id: str, request: RetryRequest) -> None:
        rec = await self.store.get_task(wi_id, self.team_run_id)
        if rec is None:
            raise RuntimeError(f"retry: {wi_id} not found")
        success = await self.store.retry_task(self.team_run_id, wi_id, rec.max_retries)
        await self.refresh_graph()
        if not success:
            self._emit(
                make_work_item_status(
                    self.team_run_id,
                    wi_id,
                    "failed",
                    failure_reason="retry_exhausted",
                )
            )

    async def request_replan(self, wi_id: str, request: ReplanRequest) -> Task:
        if self.budget_state.replans_used >= self.budgets.max_replans_per_run:
            raise BudgetExceeded("max_replans_per_run reached")

        from agents.registry import find_by_role

        replanners = find_by_role("replanner")
        if not replanners:
            raise RuntimeError("no agent with role='replanner' is registered")

        rec = await self.store.request_replan(
            self.team_run_id,
            wi_id,
            reason=request.reason,
            suggestion=request.suggestion,
            replanner_agent=replanners[0].name,
        )
        self.budget_state.tasks_used += 1
        self.budget_state.replans_used += 1
        task = _record_to_task(rec)
        self._emit(make_work_item_added(self.team_run_id, work_item_to_dict(task)))
        self._emit_budget()
        await self.refresh_graph()
        return task

    async def apply_replan(
        self,
        replan_task_id: str,
        add_tasks: list[TaskSpec],
        cancel_ids: list[str],
        target_depth: int,
        target_parent_id: str | None,
        target_root_id: str,
    ) -> dict[str, int]:
        from team.planning.validation import _has_cycle

        for cid in cancel_ids:
            rec = await self.store.get_task(cid, self.team_run_id)
            if rec is None:
                raise InvalidPlan(f"cancel target {cid} not found")
            if rec.parent_id != target_parent_id:
                raise InvalidPlan(
                    f"cancel target {cid} has parent {rec.parent_id!r}, "
                    f"but replan scoped to {target_parent_id!r}"
                )
            if rec.status not in ("pending", "ready", "expanded"):
                raise InvalidPlan(
                    f"cancel target {cid} is {rec.status}; can only cancel PENDING, READY, or EXPANDED"
                )

        local_to_new: dict[str, str] = {}
        for spec in add_tasks:
            if spec.id:
                if spec.id in local_to_new:
                    raise InvalidPlan(f"duplicate id '{spec.id}'")
                local_to_new[spec.id] = self.new_id()

        adj = await self.store.get_adjacency(self.team_run_id)
        cancelled_set = set(cancel_ids)
        clean_adj = {k: v for k, v in adj.items() if k not in cancelled_set}

        specs: list[TaskSpec] = []
        for spec in add_tasks:
            new_id = local_to_new.get(spec.id, self.new_id()) if spec.id else self.new_id()
            resolved_deps: list[str] = []
            for dep_id in spec.deps:
                if dep_id in local_to_new:
                    resolved_deps.append(local_to_new[dep_id])
                elif dep_id in adj:
                    resolved_deps.append(dep_id)
                else:
                    raise InvalidPlan(
                        f"replan dep '{dep_id}' is not a local alias or existing task id"
                    )
            clean_adj[new_id] = resolved_deps
            specs.append(
                TaskSpec(
                    id=new_id,
                    task=spec.task,
                    agent=spec.agent,
                    deps=resolved_deps,
                    scope_paths=list(spec.scope_paths),
                    cascade_policy=spec.cascade_policy,
                )
            )

        if _has_cycle(clean_adj):
            raise InvalidPlan("replan would create a cycle")

        if self.budget_state.tasks_used + len(specs) > self.budgets.max_tasks:
            raise BudgetExceeded("max_tasks would be exceeded by replan")

        await self.store.cancel_by_ids(
            self.team_run_id,
            cancel_ids,
            f"cancelled_by_replan_{replan_task_id}",
        )
        for cid in cancel_ids:
            await self.store.cascade_cancel_recursive(self.team_run_id, cid)

        if specs:
            await self.store.insert_plan(
                self.team_run_id,
                specs,
                parent_id=target_parent_id,
                parent_depth=max(0, target_depth - 1),
                parent_root_id=target_root_id or None,
            )
            self._charge_tasks(len(specs))

        await self.refresh_graph()
        return {"added": len(specs), "cancelled": len(cancel_ids)}

    async def cancel_all_pending(self) -> None:
        await self.store.cancel_all_pending(self.team_run_id)
        await self.refresh_graph()

    async def cancel_running(self, reason: str) -> None:
        await self.store.cancel_all_running(self.team_run_id, reason)
        await self.refresh_graph()

    async def all_terminal(self) -> bool:
        return await self.store.all_terminal(self.team_run_id)

    async def compute_final_statuses(self) -> set[str]:
        statuses = await self.store.get_statuses(self.team_run_id)
        return set(statuses.values())

    async def known_task_ids(self) -> set[str]:
        return await self.store.get_task_ids(self.team_run_id)

    async def done_sibling_ids(
        self,
        *,
        task_id: str,
        parent_id: str | None,
        since: float | None = None,
    ) -> list[str]:
        return await self.store.get_done_sibling_ids(
            self.team_run_id,
            task_id=task_id,
            parent_id=parent_id,
            since=since,
        )

    async def sibling_stats(self, parent_id: str | None) -> dict[str, int]:
        """Aggregate status counts for sibling tasks under the same parent."""
        return await self.store.sibling_stats(self.team_run_id, parent_id)

    async def get_task_by_id(self, task_id: str) -> Task | None:
        """Fetch a task by ID from durable storage and refresh the cache."""
        rec = await self.store.get_task(task_id, self.team_run_id)
        if rec is None:
            return None
        task = _record_to_task(rec)
        self.graph[task.id] = task
        return task

    async def checkpoint(
        self,
        label: str | None,
        project_context: Any,
    ) -> TeamRunCheckpoint:
        await self.refresh_graph()
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
                project_context=copy.deepcopy(project_context),
                budget_state=copy.deepcopy(self.budget_state),
            )
            self._checkpoints.append(cp)
            # Persist to PG for crash recovery
            if self._checkpoint_store is not None and getattr(
                self._checkpoint_store, "initialized", False
            ):
                try:
                    await self._checkpoint_store.save(cp)
                except Exception:
                    _logger.debug(
                        "Failed to persist checkpoint %s", cp.id, exc_info=True
                    )
            self._emit(
                make_checkpoint_taken(
                    self.team_run_id,
                    checkpoint_id=cp.id,
                    sequence=cp.sequence,
                    label=label,
                )
            )
            return cp

    def list_checkpoints(self) -> list[TeamRunCheckpoint]:
        return list(self._checkpoints)

    def _get_checkpoint(self, checkpoint_id: str) -> TeamRunCheckpoint | None:
        return next((cp for cp in self._checkpoints if cp.id == checkpoint_id), None)

    async def _get_checkpoint_with_fallback(
        self, checkpoint_id: str
    ) -> TeamRunCheckpoint | None:
        """Check in-memory cache first, then fall back to PG."""
        cp = self._get_checkpoint(checkpoint_id)
        if cp is not None:
            return cp
        if self._checkpoint_store is not None and getattr(
            self._checkpoint_store, "initialized", False
        ):
            from team.persistence.checkpoint_store import CheckpointRecord
            rec = await self._checkpoint_store.load_by_id(
                checkpoint_id, self.team_run_id
            )
            if rec is not None:
                return self._record_to_checkpoint(rec)
        return None

    @staticmethod
    def _record_to_checkpoint(rec: Any) -> TeamRunCheckpoint:
        """Reconstruct a TeamRunCheckpoint from a CheckpointRecord."""
        from team.models import BudgetState, Task, TaskStatus
        work_items: dict[str, Task] = {}
        for task_id, task_data in (rec.work_items or {}).items():
            # Reconstruct datetime fields
            for field in ("created_at", "started_at", "finished_at"):
                val = task_data.get(field)
                if isinstance(val, str) and val:
                    from datetime import datetime
                    try:
                        task_data[field] = datetime.fromisoformat(val)
                    except ValueError:
                        task_data[field] = None
                elif not isinstance(val, datetime):
                    task_data[field] = None
            if "status" in task_data:
                task_data["status"] = TaskStatus(task_data["status"])
            work_items[task_id] = Task(**task_data)
        budget = BudgetState(**(rec.budget_state or {}))
        return TeamRunCheckpoint(
            id=rec.id,
            team_run_id=rec.team_run_id,
            sequence=rec.sequence,
            taken_at=rec.taken_at,
            label=rec.label,
            work_items=work_items,
            ready_queue_order=list(rec.ready_queue_order or []),
            project_context=rec.project_context,
            budget_state=budget,
        )

    async def rollback_to(
        self,
        checkpoint_id: str,
        project_context_setter: Callable[[Any], None],
    ) -> TeamRunCheckpoint:
        cp = await self._get_checkpoint_with_fallback(checkpoint_id)
        if cp is None:
            raise CheckpointNotFound(checkpoint_id)
        await self.store.replace_run_tasks(self.team_run_id, list(cp.work_items.values()))
        self.graph = copy.deepcopy(cp.work_items)
        self._ready_order = list(cp.ready_queue_order)
        self.budget_state = copy.deepcopy(cp.budget_state)
        project_context_setter(copy.deepcopy(cp.project_context))
        return cp

    async def prepare_for_resume(self) -> None:
        if self._resume_snapshot is not None:
            await self.store.replace_run_tasks(self.team_run_id, self._resume_snapshot)
            self._resume_snapshot = None
        recovered = await self.store.recover_running(self.team_run_id)
        if recovered:
            _logger.info("Recovered %d running tasks to ready", len(recovered))
        await self.refresh_graph()
