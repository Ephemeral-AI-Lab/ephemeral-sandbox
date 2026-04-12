"""Dispatcher — DAG, ready queue, and atomic mutations for one TeamRun.

When a PGDispatcher is provided, PostgreSQL is the single source of truth.
No in-memory graph. All operations delegate to SQL.

When no PG is available (pg=None), the in-memory graph + asyncio.Queue
path is used (single-process mode).
"""

from __future__ import annotations

import asyncio
import copy
import logging
import uuid
from collections import deque
from typing import TYPE_CHECKING, Any, Callable

from team.errors import (
    BudgetExceeded,
    InvalidPlan,
)
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    ReplanRequest,
    RetryRequest,
    Task,
    TaskSpec,
    TaskStatus,
    TERMINAL_STATUSES,
    _utcnow,
)
from team.persistence.events import (
    TeamRunEvent,
    make_budget_update,
    make_work_item_added,
    make_work_item_status,
    work_item_to_dict,
)
from team.persistence.run_store import NullTeamRunStore, TeamRunStore
from team.planning.validation import validate_plan
from team.runtime.checkpoint import TeamRunCheckpoint

if TYPE_CHECKING:
    from team.runtime.pg_dispatcher import PGDispatcher

_logger = logging.getLogger(__name__)


def _record_to_task(rec: Any) -> Task:
    """Convert a PG TaskRecord to a Task dataclass."""
    return Task(
        id=rec.id, team_run_id=rec.team_run_id,
        agent_name=rec.agent_name,
        status=TaskStatus(rec.status),
        task=rec.task,
        deps=list(rec.deps) if rec.deps else [],
        scope_paths=list(rec.scope_paths) if rec.scope_paths else [],
        cascade_policy=rec.cascade_policy or "cancel",
        parent_id=rec.parent_id, root_id=rec.root_id or "",
        depth=rec.depth or 0, retry_count=rec.retry_count or 0,
        max_retries=rec.max_retries or 2,
        agent_run_id=rec.agent_run_id,
        created_at=rec.created_at or _utcnow(),
        started_at=rec.started_at, finished_at=rec.finished_at,
        failure_reason=rec.failure_reason,
    )


class Dispatcher:
    """Owns the Task DAG for one TeamRun.

    When ``pg`` is provided: PG is the single source of truth.
    ``self.graph`` is NOT used. All ops go directly to SQL.

    When ``pg`` is None: in-memory graph + asyncio.Queue (single-process).
    """

    def __init__(
        self,
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        max_checkpoints: int = 10,
        event_store: TeamRunStore | None = None,
        pg: "PGDispatcher | None" = None,
    ) -> None:
        self.team_run_id = team_run_id
        self.budgets = budgets
        self.budget_state = budget_state
        # In-memory state — only used when pg is None
        self.graph: dict[str, Task] = {}
        self._ready_queue: asyncio.Queue[str] = asyncio.Queue()
        self._ready_order: list[str] = []
        self.lock = asyncio.Lock()
        self._checkpoints: deque[TeamRunCheckpoint] = deque(maxlen=max_checkpoints)
        self._checkpoint_seq = 0
        self._events: TeamRunStore = event_store or NullTeamRunStore()
        self._pg: "PGDispatcher | None" = pg
        # Set by TeamRun after construction so cascade "continue" can inject notes
        self.task_center: Any = None

    @property
    def pg_enabled(self) -> bool:
        return self._pg is not None

    # ---- event emission --------------------------------------------------

    def _emit(self, event: TeamRunEvent) -> None:
        try:
            self._events.append(event)
        except Exception:
            _logger.exception("team event store append failed; continuing")

    def _emit_budget(self) -> None:
        self._emit(make_budget_update(
            self.team_run_id,
            tasks_used=self.budget_state.tasks_used,
            note_bytes_used=self.budget_state.note_bytes_used,
            replans_used=self.budget_state.replans_used,
        ))

    def new_id(self) -> str:
        return str(uuid.uuid4())

    # ==================================================================
    # PG PATH — no self.graph, all SQL
    # ==================================================================

    async def _pg_add_work_item(self, wi: Task) -> None:
        assert self._pg is not None
        if self.budget_state.tasks_used >= self.budgets.max_tasks:
            raise BudgetExceeded(f"max_tasks={self.budgets.max_tasks} reached")
        await self._pg.insert_plan(
            self.team_run_id,
            [TaskSpec(
                id=wi.id, task=wi.task, agent=wi.agent_name,
                deps=list(wi.deps), scope_paths=list(wi.scope_paths),
                cascade_policy=wi.cascade_policy,
            )],
            parent_id=wi.parent_id,
            parent_depth=max(0, wi.depth - 1) if wi.parent_id else 0,
            parent_root_id=wi.root_id or None,
        )
        self.budget_state.tasks_used += 1
        self._emit(make_work_item_added(self.team_run_id, work_item_to_dict(wi)))
        self._emit_budget()

    async def _pg_pop_ready(self) -> str:
        assert self._pg is not None
        while True:
            rec = await self._pg.pop_ready(self.team_run_id)
            if rec is not None:
                return rec.id
            await asyncio.sleep(0.05)

    async def _pg_mark_running(self, wi_id: str, agent_run_id: str) -> Task:
        assert self._pg is not None
        rec = await self._pg.mark_running(self.team_run_id, wi_id, agent_run_id)
        if rec is None:
            raise RuntimeError(f"mark_running: {wi_id} not found in PG")
        task = _record_to_task(rec)
        self._emit(make_work_item_status(
            self.team_run_id, wi_id, "running",
            agent_run_id=agent_run_id,
            started_at=task.started_at.isoformat() if task.started_at else None,
        ))
        return task

    async def _pg_complete(self, wi_id: str, result: AgentResult) -> list[Task]:
        assert self._pg is not None
        new_items: list[Task] = []

        rec = await self._pg.get_task(wi_id, self.team_run_id)
        if rec is None or rec.status != "running":
            raise RuntimeError(
                f"complete: {wi_id} is {rec.status if rec else 'missing'}, not RUNNING"
            )

        from agents.registry import has_role as _has_role
        if _has_role(rec.agent_name, "planner") and result.submitted_plan is None:
            await self._pg.mark_failed(
                wi_id, self.team_run_id,
                "InvalidPlan: expandable work item did not submit a plan",
            )
            await self._pg.cascade_cancel_recursive(self.team_run_id, wi_id)
            return []

        if result.submitted_plan is not None:
            new_depth = (rec.depth or 0) + 1
            if new_depth > self.budgets.max_depth:
                await self._pg.mark_failed(
                    wi_id, self.team_run_id,
                    f"InvalidPlan: plan would exceed max_depth={self.budgets.max_depth}",
                )
                await self._pg.cascade_cancel_recursive(self.team_run_id, wi_id)
                return []

            # Validate plan — need adjacency for known_external_deps
            adj = await self._pg.get_adjacency(self.team_run_id)
            issues = validate_plan(
                result.submitted_plan,
                max_plan_size=self.budgets.max_plan_size,
                known_external_deps=set(adj.keys()),
            )
            if issues:
                await self._pg.mark_failed(
                    wi_id, self.team_run_id,
                    "InvalidPlan: " + "; ".join(i["msg"] for i in issues),
                )
                await self._pg.cascade_cancel_recursive(self.team_run_id, wi_id)
                return []

            # Resolve local → global ids
            local_to_global: dict[str, str] = {
                spec.id: self.new_id()
                for spec in result.submitted_plan.tasks
                if spec.id
            }
            pg_specs: list[TaskSpec] = []
            for spec in result.submitted_plan.tasks:
                new_id = local_to_global.get(spec.id) or self.new_id()
                resolved_deps = [
                    local_to_global[d] if d in local_to_global else d
                    for d in spec.deps
                ]
                pg_specs.append(TaskSpec(
                    id=new_id, task=spec.task, agent=spec.agent,
                    deps=resolved_deps, scope_paths=list(spec.scope_paths),
                    cascade_policy=spec.cascade_policy,
                ))
                new_items.append(Task(
                    id=new_id, team_run_id=self.team_run_id,
                    agent_name=spec.agent, status=TaskStatus.PENDING,
                    task=spec.task, deps=resolved_deps,
                    scope_paths=list(spec.scope_paths),
                    cascade_policy=spec.cascade_policy,
                    parent_id=wi_id, root_id=rec.root_id or wi_id,
                    depth=new_depth,
                ))

            if self.budget_state.tasks_used + len(new_items) > self.budgets.max_tasks:
                await self._pg.mark_failed(
                    wi_id, self.team_run_id, "BudgetExceeded: max_tasks")
                await self._pg.cascade_cancel_recursive(self.team_run_id, wi_id)
                return []

            await self._pg.insert_plan(
                self.team_run_id, pg_specs,
                parent_id=wi_id,
                parent_depth=rec.depth or 0,
                parent_root_id=rec.root_id or wi_id,
            )
            self.budget_state.tasks_used += len(new_items)
            for nwi in new_items:
                self._emit(make_work_item_added(
                    self.team_run_id, work_item_to_dict(nwi)))
            self._emit_budget()

        # Mark done (atomically promotes dependents via pending_dep_count)
        await self._pg.mark_done(wi_id, self.team_run_id)
        self._emit(make_work_item_status(
            self.team_run_id, wi_id, "done",
            finished_at=_utcnow().isoformat(),
        ))

        # Handle inline replan
        if result.submitted_replan is not None:
            await self._pg_apply_replan(
                replan_task_id=wi_id,
                add_tasks=result.submitted_replan.add_tasks,
                cancel_ids=result.submitted_replan.cancel_ids,
                target_depth=rec.depth or 0,
                target_parent_id=rec.parent_id,
                target_root_id=rec.root_id or "",
            )

        return new_items

    async def _pg_fail(self, wi_id: str, reason: str) -> None:
        assert self._pg is not None
        await self._pg.fail_task(self.team_run_id, wi_id, reason)

    async def _pg_retry(self, wi_id: str, request: RetryRequest) -> None:
        assert self._pg is not None
        rec = await self._pg.get_task(wi_id, self.team_run_id)
        if rec is None:
            raise RuntimeError(f"retry: {wi_id} not found")
        success = await self._pg.retry_task(
            self.team_run_id, wi_id, rec.max_retries)
        if not success:
            self._emit(make_work_item_status(
                self.team_run_id, wi_id, "failed",
                failure_reason="retry_exhausted",
            ))

    async def _pg_request_replan(self, wi_id: str, request: ReplanRequest) -> Task:
        assert self._pg is not None
        if self.budget_state.replans_used >= self.budgets.max_replans_per_run:
            raise BudgetExceeded("max_replans_per_run reached")

        from agents.registry import find_by_role
        replanners = find_by_role("replanner")
        if not replanners:
            raise RuntimeError("no agent with role='replanner' is registered")

        rec = await self._pg.request_replan(
            self.team_run_id, wi_id,
            reason=request.reason,
            suggestion=request.suggestion,
            replanner_agent=replanners[0].name,
        )
        self.budget_state.tasks_used += 1
        self.budget_state.replans_used += 1
        self._emit(make_work_item_added(
            self.team_run_id, work_item_to_dict(_record_to_task(rec))))
        self._emit_budget()
        return _record_to_task(rec)

    async def _pg_apply_replan(
        self, *,
        replan_task_id: str,
        add_tasks: list[TaskSpec],
        cancel_ids: list[str],
        target_depth: int,
        target_parent_id: str | None,
        target_root_id: str,
    ) -> dict[str, int]:
        assert self._pg is not None
        from team.planning.validation import _has_cycle

        # Validate cancel targets exist and are cancellable
        for cid in cancel_ids:
            rec = await self._pg.get_task(cid, self.team_run_id)
            if rec is None:
                raise InvalidPlan(f"cancel target {cid} not found")
            if rec.parent_id != target_parent_id:
                raise InvalidPlan(
                    f"cancel target {cid} has parent {rec.parent_id!r}, "
                    f"but replan scoped to {target_parent_id!r}")
            if rec.status not in ("pending", "ready"):
                raise InvalidPlan(
                    f"cancel target {cid} is {rec.status}; "
                    f"can only cancel PENDING or READY")

        # Resolve ids
        local_to_new: dict[str, str] = {}
        for spec in add_tasks:
            if spec.id:
                if spec.id in local_to_new:
                    raise InvalidPlan(f"duplicate id '{spec.id}'")
                local_to_new[spec.id] = self.new_id()

        # Build adjacency for cycle check
        adj = await self._pg.get_adjacency(self.team_run_id)
        cancelled_set = set(cancel_ids)
        clean_adj = {k: v for k, v in adj.items() if k not in cancelled_set}

        pg_specs: list[TaskSpec] = []
        for spec in add_tasks:
            new_id = local_to_new.get(spec.id, self.new_id()) if spec.id else self.new_id()
            resolved_deps: list[str] = []
            for d in spec.deps:
                if d in local_to_new:
                    resolved_deps.append(local_to_new[d])
                elif d in adj:
                    resolved_deps.append(d)
                else:
                    raise InvalidPlan(
                        f"replan dep '{d}' is not a local alias or existing task id")
            clean_adj[new_id] = resolved_deps
            pg_specs.append(TaskSpec(
                id=new_id, task=spec.task, agent=spec.agent,
                deps=resolved_deps, scope_paths=list(spec.scope_paths),
                cascade_policy=spec.cascade_policy,
            ))

        if _has_cycle(clean_adj):
            raise InvalidPlan("replan would create a cycle")

        if self.budget_state.tasks_used + len(pg_specs) > self.budgets.max_tasks:
            raise BudgetExceeded("max_tasks would be exceeded by replan")

        # Execute: cancel targets, cascade, insert new
        await self._pg.cancel_by_ids(
            self.team_run_id, cancel_ids,
            f"cancelled_by_replan_{replan_task_id}")
        for cid in cancel_ids:
            await self._pg.cascade_cancel_recursive(self.team_run_id, cid)

        if pg_specs:
            await self._pg.insert_plan(
                self.team_run_id, pg_specs,
                parent_id=target_parent_id,
                parent_depth=max(0, target_depth - 1),
                parent_root_id=target_root_id or None,
            )
            self.budget_state.tasks_used += len(pg_specs)
            self._emit_budget()

        return {"added": len(pg_specs), "cancelled": len(cancel_ids)}

    async def _pg_cancel_all_pending(self) -> None:
        assert self._pg is not None
        await self._pg.cancel_all_pending(self.team_run_id)

    async def _pg_cancel_running(self, reason: str) -> None:
        assert self._pg is not None
        await self._pg.cancel_all_running(self.team_run_id, reason)

    async def _pg_all_terminal(self) -> bool:
        assert self._pg is not None
        return await self._pg.all_terminal(self.team_run_id)

    async def _pg_compute_final_statuses(self) -> set[str]:
        """Get all distinct statuses from PG for final status computation."""
        assert self._pg is not None
        statuses = await self._pg.get_statuses(self.team_run_id)
        return set(statuses.values())

    # ==================================================================
    # IN-MEMORY PATH (no PG) — unchanged legacy behavior
    # ==================================================================

    def _mark_failed(self, wi: Task, reason: str) -> None:
        wi.status = TaskStatus.FAILED
        wi.finished_at = _utcnow()
        wi.failure_reason = reason
        self._emit(make_work_item_status(
            self.team_run_id, wi.id, "failed",
            finished_at=wi.finished_at.isoformat() if wi.finished_at else None,
            failure_reason=wi.failure_reason,
        ))

    def _mark_cancelled(self, wi: Task, reason: str) -> None:
        wi.status = TaskStatus.CANCELLED
        wi.finished_at = _utcnow()
        wi.failure_reason = reason
        self._emit(make_work_item_status(
            self.team_run_id, wi.id, "cancelled",
            finished_at=wi.finished_at.isoformat(),
            failure_reason=wi.failure_reason,
        ))

    def _compute_readiness(self, wi: Task) -> bool:
        if wi.status != TaskStatus.PENDING:
            return False
        for dep_id in wi.deps:
            if not self._dependency_satisfied(dep_id):
                return False
        return True

    def _ancestor_ids(self, wi_id: str) -> list[str]:
        ancestors: list[str] = []
        seen: set[str] = set()
        current = self.graph.get(wi_id)
        while current is not None and current.parent_id:
            parent_id = current.parent_id
            if parent_id in seen:
                break
            ancestors.append(parent_id)
            seen.add(parent_id)
            current = self.graph.get(parent_id)
        return ancestors

    def _dependency_root_ids(self, wi_id: str) -> list[str]:
        return [wi_id, *self._ancestor_ids(wi_id)]

    def _subtree_ids(self, root_id: str) -> list[str]:
        ordered: list[str] = []
        stack = [root_id]
        seen: set[str] = set()
        while stack:
            current_id = stack.pop()
            if current_id in seen:
                continue
            seen.add(current_id)
            ordered.append(current_id)
            child_ids = [
                child.id for child in self.graph.values()
                if child.parent_id == current_id
            ]
            stack.extend(reversed(child_ids))
        return ordered

    def _dependency_satisfied(self, dep_id: str) -> bool:
        dep = self.graph.get(dep_id)
        if dep is None or dep.status != TaskStatus.DONE:
            return False
        for node_id in self._subtree_ids(dep_id):
            node = self.graph.get(node_id)
            if node is None:
                return False
            if node.status == TaskStatus.FAILED:
                return False
            if node.status not in (TaskStatus.DONE, TaskStatus.CANCELLED):
                return False
        return True

    def _cancel_superseded_dependency_validators(self, wi: Task) -> None:
        from agents.registry import has_role
        if not has_role(wi.agent_name, "reviewer") or wi.status not in (
            TaskStatus.PENDING, TaskStatus.READY, TaskStatus.RUNNING,
        ):
            return
        for node_id in {node for dep_id in wi.deps
                        for node in self._subtree_ids(dep_id)}:
            node = self.graph.get(node_id)
            if (node_id != wi.id and node
                    and has_role(node.agent_name, "reviewer")
                    and node.status == TaskStatus.FAILED):
                self._mark_cancelled(
                    node, f"superseded_by_active_validator_{wi.id}")

    def _promote_ready_work_items(self) -> None:
        for candidate in list(self.graph.values()):
            self._cancel_superseded_dependency_validators(candidate)
            if self._compute_readiness(candidate):
                self._promote_to_ready(candidate)

    def _enqueue(self, wi: Task) -> None:
        wi.status = TaskStatus.READY
        self._ready_queue.put_nowait(wi.id)
        self._ready_order.append(wi.id)
        self._emit(make_work_item_status(self.team_run_id, wi.id, "ready"))

    def _promote_to_ready(self, wi: Task) -> None:
        assert wi.status == TaskStatus.PENDING
        self._enqueue(wi)

    # ==================================================================
    # PUBLIC API — delegates to PG or in-memory
    # ==================================================================

    async def add_work_item(self, wi: Task) -> None:
        if self._pg is not None:
            await self._pg_add_work_item(wi)
            return
        async with self.lock:
            if self.budget_state.tasks_used >= self.budgets.max_tasks:
                raise BudgetExceeded(
                    f"max_tasks={self.budgets.max_tasks} reached")
            if wi.id in self.graph:
                raise ValueError(f"Task {wi.id} already exists")
            self.graph[wi.id] = wi
            self.budget_state.tasks_used += 1
            self._emit(make_work_item_added(
                self.team_run_id, work_item_to_dict(wi)))
            self._emit_budget()
            if self._compute_readiness(wi):
                self._promote_to_ready(wi)

    async def pop_ready(self) -> str:
        if self._pg is not None:
            return await self._pg_pop_ready()
        while True:
            wi_id = await self._ready_queue.get()
            async with self.lock:
                try:
                    self._ready_order.remove(wi_id)
                except ValueError:
                    pass
                wi = self.graph.get(wi_id)
                if wi is None or wi.status != TaskStatus.READY:
                    continue
                return wi_id

    async def mark_running(self, wi_id: str, agent_run_id: str) -> Task:
        if self._pg is not None:
            return await self._pg_mark_running(wi_id, agent_run_id)
        async with self.lock:
            wi = self.graph[wi_id]
            if wi.status != TaskStatus.READY:
                raise RuntimeError(
                    f"mark_running: {wi_id} is {wi.status.value}, not READY")
            wi.status = TaskStatus.RUNNING
            wi.agent_run_id = agent_run_id
            wi.started_at = _utcnow()
            self._emit(make_work_item_status(
                self.team_run_id, wi_id, "running",
                agent_run_id=agent_run_id,
                started_at=wi.started_at.isoformat(),
            ))
            return wi

    async def complete(self, wi_id: str, result: AgentResult) -> list[Task]:
        if self._pg is not None:
            return await self._pg_complete(wi_id, result)
        # In-memory path
        from team.runtime.dispatcher_mutation_ops import cascade_cancel_dependency_subtree
        new_items: list[Task] = []
        async with self.lock:
            wi = self.graph[wi_id]
            if wi.status != TaskStatus.RUNNING:
                raise RuntimeError(
                    f"complete: {wi_id} is {wi.status.value}, not RUNNING")

            from agents.registry import has_role as _has_role_check
            if _has_role_check(wi.agent_name, "planner") and result.submitted_plan is None:
                self._mark_failed(
                    wi, "InvalidPlan: expandable work item did not submit a plan")
                cascade_cancel_dependency_subtree(self, wi_id)
                return []

            if result.submitted_plan is not None:
                new_depth = wi.depth + 1
                if new_depth > self.budgets.max_depth:
                    self._mark_failed(
                        wi, f"InvalidPlan: plan would exceed max_depth={self.budgets.max_depth}")
                    cascade_cancel_dependency_subtree(self, wi_id)
                    return []
                issues = validate_plan(
                    result.submitted_plan,
                    max_plan_size=self.budgets.max_plan_size,
                    known_external_deps=set(self.graph.keys()),
                )
                if issues:
                    self._mark_failed(
                        wi, "InvalidPlan: " + "; ".join(i["msg"] for i in issues))
                    cascade_cancel_dependency_subtree(self, wi_id)
                    return []
                local_to_global: dict[str, str] = {
                    spec.id: self.new_id()
                    for spec in result.submitted_plan.tasks if spec.id
                }
                for spec in result.submitted_plan.tasks:
                    new_id = local_to_global.get(spec.id) or self.new_id()
                    resolved_deps = [
                        local_to_global[d] if d in local_to_global else d
                        for d in spec.deps
                    ]
                    new_items.append(Task(
                        id=new_id, team_run_id=self.team_run_id,
                        agent_name=spec.agent, status=TaskStatus.PENDING,
                        task=spec.task, deps=resolved_deps,
                        scope_paths=list(spec.scope_paths),
                        cascade_policy=spec.cascade_policy,
                        parent_id=wi.id, root_id=wi.root_id or wi.id,
                        depth=new_depth,
                    ))
                if self.budget_state.tasks_used + len(new_items) > self.budgets.max_tasks:
                    self._mark_failed(wi, "BudgetExceeded: max_tasks")
                    cascade_cancel_dependency_subtree(self, wi_id)
                    return []

            for nwi in new_items:
                self.graph[nwi.id] = nwi
                self.budget_state.tasks_used += 1
                self._emit(make_work_item_added(
                    self.team_run_id, work_item_to_dict(nwi)))
            if new_items:
                self._emit_budget()

            wi.status = TaskStatus.DONE
            wi.finished_at = _utcnow()
            self._emit(make_work_item_status(
                self.team_run_id, wi_id, "done",
                finished_at=wi.finished_at.isoformat(),
            ))
            self._promote_ready_work_items()

            if result.submitted_replan is not None:
                from team.runtime.dispatcher_replan_ops import apply_replan_unlocked
                apply_replan_unlocked(
                    self,
                    replan_task_id=wi_id,
                    add_tasks=result.submitted_replan.add_tasks,
                    cancel_ids=result.submitted_replan.cancel_ids,
                    target_depth=wi.depth,
                    target_parent_id=wi.parent_id,
                    target_root_id=wi.root_id,
                )

        return new_items

    async def fail(self, wi_id: str, reason: str) -> None:
        if self._pg is not None:
            await self._pg_fail(wi_id, reason)
            return
        from team.runtime.dispatcher_mutation_ops import fail as fail_work_item
        await fail_work_item(self, wi_id=wi_id, reason=reason)

    async def retry_work_item(self, wi_id: str, request: RetryRequest) -> None:
        if self._pg is not None:
            await self._pg_retry(wi_id, request)
            return
        from team.runtime.dispatcher_mutation_ops import (
            retry_work_item as retry_dispatcher_work_item,
        )
        await retry_dispatcher_work_item(self, wi_id=wi_id, request=request)

    async def request_replan(self, wi_id: str, request: ReplanRequest) -> Task:
        if self._pg is not None:
            return await self._pg_request_replan(wi_id, request)
        from team.runtime.dispatcher_replan_ops import (
            request_replan as request_dispatcher_replan,
        )
        return await request_dispatcher_replan(self, wi_id=wi_id, request=request)

    async def cancel_all_pending(self) -> None:
        if self._pg is not None:
            await self._pg_cancel_all_pending()
            return
        from team.runtime.dispatcher_mutation_ops import (
            cancel_all_pending as cancel_dispatcher_pending,
        )
        await cancel_dispatcher_pending(self)

    async def cancel_running(self, reason: str) -> None:
        if self._pg is not None:
            await self._pg_cancel_running(reason)
            return
        from team.runtime.dispatcher_mutation_ops import (
            cancel_running as cancel_dispatcher_running,
        )
        await cancel_dispatcher_running(self, reason=reason)

    def all_terminal(self) -> bool:
        """Sync version — in-memory only. PG callers use all_terminal_async."""
        return all(wi.status in TERMINAL_STATUSES for wi in self.graph.values())

    async def all_terminal_async(self) -> bool:
        if self._pg is not None:
            return await self._pg_all_terminal()
        return all(wi.status in TERMINAL_STATUSES for wi in self.graph.values())

    async def compute_final_statuses(self) -> set[str]:
        """Get all distinct statuses for final run status computation."""
        if self._pg is not None:
            return await self._pg_compute_final_statuses()
        return {str(wi.status.value) for wi in self.graph.values()}

    async def known_task_ids(self) -> set[str]:
        """Return the current task IDs in the run."""
        if self._pg is not None:
            assert self._pg is not None
            return await self._pg.get_task_ids(self.team_run_id)
        return set(self.graph.keys())

    async def done_sibling_ids(
        self,
        *,
        task_id: str,
        parent_id: str | None,
        since: float | None = None,
    ) -> list[str]:
        """Return sibling task IDs that completed since the given time."""
        if self._pg is not None:
            assert self._pg is not None
            return await self._pg.get_done_sibling_ids(
                self.team_run_id,
                task_id=task_id,
                parent_id=parent_id,
                since=since,
            )
        done_ids: list[str] = []
        for wi in self.graph.values():
            if wi.id == task_id or wi.parent_id != parent_id or wi.status != TaskStatus.DONE:
                continue
            if since is not None:
                finished_at = wi.finished_at.timestamp() if wi.finished_at else 0.0
                if finished_at < since:
                    continue
            done_ids.append(wi.id)
        return done_ids

    # ---- checkpoint / rollback -------------------------------------------

    async def checkpoint(
        self,
        label: str | None,
        project_context: Any,
    ) -> TeamRunCheckpoint:
        if self._pg is not None:
            # Load all tasks from PG for the checkpoint snapshot
            all_tasks = await self._pg.get_all_tasks(self.team_run_id)
            task_dict = {r.id: _record_to_task(r) for r in all_tasks}
        else:
            task_dict = self.graph

        async with self.lock:
            self._checkpoint_seq += 1
            cp = TeamRunCheckpoint(
                id=str(uuid.uuid4()),
                team_run_id=self.team_run_id,
                sequence=self._checkpoint_seq,
                taken_at=_utcnow(),
                label=label,
                work_items=copy.deepcopy(task_dict),
                ready_queue_order=list(self._ready_order),
                project_context=copy.deepcopy(project_context),
                budget_state=copy.deepcopy(self.budget_state),
            )
            self._checkpoints.append(cp)
            from team.persistence.events import make_checkpoint_taken
            self._emit(make_checkpoint_taken(
                self.team_run_id,
                checkpoint_id=cp.id,
                sequence=cp.sequence,
                label=label,
            ))
            return cp

    def list_checkpoints(self) -> list[TeamRunCheckpoint]:
        return list(self._checkpoints)

    def _get_checkpoint(self, checkpoint_id: str) -> TeamRunCheckpoint | None:
        return next(
            (cp for cp in self._checkpoints if cp.id == checkpoint_id), None)

    async def rollback_to(
        self,
        checkpoint_id: str,
        project_context_setter: Callable[[Any], None],
    ) -> TeamRunCheckpoint:
        from team.runtime.dispatcher_checkpoint_ops import (
            rollback_to as rollback_dispatcher_state,
        )
        return await rollback_dispatcher_state(
            self,
            checkpoint_id=checkpoint_id,
            project_context_setter=project_context_setter,
        )

    async def prepare_for_resume(self) -> None:
        if self._pg is not None:
            recovered = await self._pg.recover_running(self.team_run_id)
            if recovered:
                _logger.info(
                    "PG crash recovery: reset %d running tasks to ready",
                    len(recovered))
            return
        from team.runtime.dispatcher_checkpoint_ops import (
            prepare_for_resume as prepare_dispatcher_for_resume,
        )
        await prepare_dispatcher_for_resume(self)

    async def apply_replan(
        self,
        replan_task_id: str,
        add_tasks: list[TaskSpec],
        cancel_ids: list[str],
        target_depth: int,
        target_parent_id: str | None,
        target_root_id: str,
    ) -> dict[str, int]:
        if self._pg is not None:
            return await self._pg_apply_replan(
                replan_task_id=replan_task_id,
                add_tasks=add_tasks, cancel_ids=cancel_ids,
                target_depth=target_depth,
                target_parent_id=target_parent_id,
                target_root_id=target_root_id,
            )
        from team.runtime.dispatcher_replan_ops import apply_replan_unlocked
        async with self.lock:
            return apply_replan_unlocked(
                self,
                replan_task_id=replan_task_id,
                add_tasks=add_tasks, cancel_ids=cancel_ids,
                target_depth=target_depth,
                target_parent_id=target_parent_id,
                target_root_id=target_root_id,
            )
