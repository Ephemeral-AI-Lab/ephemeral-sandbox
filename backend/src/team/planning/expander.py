"""PlanExpander — submitted-plan validation, ID remapping, replan application.

Extracted from TaskCenter. Owns:
- Validating submitted plans (depth, size, cycles, budget)
- Remapping local plan IDs to global UUIDs
- Inserting expanded children into the task graph
- Applying replans (cancel allowed graph-region tasks + add new tasks)

Validation failures during plan expansion cascade-cancel the parent via
``cascade_fail_cb`` and return ``ok=False``; they do not raise. Replan
validation failures (apply_replan) raise InvalidPlan/BudgetExceeded so the
caller can surface them to the requester.
"""

from __future__ import annotations

import logging
import uuid
from typing import Awaitable, Callable

from agents.registry import has_role
from team.budget_manager import BudgetManager
from team.errors import BudgetExceeded, InvalidPlan
from team.models import AgentResult, Task, TaskDefinition, TaskStatus
from team.persistence.events import TeamRunEvent, make_task_added, task_to_dict
from team.persistence.task_record import TaskRecord
from team.persistence.task_store import TaskStore
from team.planning.validation import _has_cycle, validate_plan

logger = logging.getLogger(__name__)


class PlanExpander:
    """Validates and applies submitted plans + replans for TaskCenter."""

    def __init__(
        self,
        *,
        team_run_id: str,
        store: TaskStore,
        budget: BudgetManager,
        graph_getter: Callable[[], dict[str, Task]],
        emit_cb: Callable[[TeamRunEvent], None],
        cascade_fail_cb: Callable[[str, str], Awaitable[None]],
        cancel_active_task_cb: Callable[[str], bool] | None = None,
    ) -> None:
        self._team_run_id = team_run_id
        self._store = store
        self._budget = budget
        self._graph_getter = graph_getter
        self._emit = emit_cb
        self._cascade_fail = cascade_fail_cb
        self._cancel_active_task = cancel_active_task_cb

    @staticmethod
    def new_id() -> str:
        return str(uuid.uuid4())

    async def expand_submitted_plan(
        self,
        rec: TaskRecord,
        result: AgentResult,
    ) -> tuple[list[Task], bool]:
        """Validate and insert children for a submitted plan.

        Returns ``(new_items, ok)``. When ``ok`` is False the parent has
        been cascade-failed and the caller should treat the completion as
        terminated. When the agent submitted no plan and no plan was
        required, returns ``([], True)``.
        """
        task_id = rec.id

        if has_role(rec.agent_name, "planner") and result.submitted_plan is None:
            await self._cascade_fail(task_id, "InvalidPlan: expandable task did not submit a plan")
            return [], False

        if result.submitted_plan is None:
            return [], True

        new_depth = (rec.depth or 0) + 1
        if not self._budget.within_depth_limit(new_depth):
            await self._cascade_fail(
                task_id,
                f"InvalidPlan: plan would exceed max_depth={self._budget.budgets.max_depth} "
                f"(current depth={rec.depth or 0}). Planners at the depth limit must "
                f"emit developer tasks with broader scopes instead of nested team_planner tasks.",
            )
            return [], False

        adj = await self._store.get_adjacency()
        allow_empty = bool(rec.root_id) and task_id != (rec.root_id or task_id)
        issues = validate_plan(
            result.submitted_plan,
            max_plan_size=self._budget.budgets.max_plan_size,
            allow_empty=allow_empty,
            known_external_deps=set(adj.keys()),
        )
        if issues:
            await self._cascade_fail(task_id, "InvalidPlan: " + "; ".join(i["msg"] for i in issues))
            return [], False

        local_to_global: dict[str, str] = {
            spec.id: self.new_id() for spec in result.submitted_plan.tasks if spec.id
        }
        specs: list[TaskDefinition] = []
        new_items: list[Task] = []
        for spec in result.submitted_plan.tasks:
            nid = local_to_global.get(spec.id) or self.new_id()
            rdeps = [local_to_global[d] if d in local_to_global else d for d in spec.deps]
            specs.append(
                TaskDefinition(
                    id=nid,
                    objective=spec.objective,
                    agent=spec.agent,
                    description=spec.description or "",
                    deps=rdeps,
                    scope_paths=list(spec.scope_paths),
                )
            )
            new_items.append(
                Task(
                    id=nid,
                    team_run_id=self._team_run_id,
                    agent_name=spec.agent,
                    status=TaskStatus.READY if not rdeps else TaskStatus.PENDING,
                    objective=spec.objective,
                    description=spec.description or "",
                    deps=rdeps,
                    scope_paths=list(spec.scope_paths),
                    parent_id=task_id,
                    root_id=rec.root_id or task_id,
                    depth=new_depth,
                )
            )

        if not self._budget.has_capacity_for(len(new_items)):
            await self._cascade_fail(task_id, "BudgetExceeded: max_tasks")
            return [], False

        inserted = await self._store.insert_plan(
            specs,
            parent_id=task_id,
            parent_depth=rec.depth or 0,
            parent_root_id=rec.root_id or task_id,
        )
        self._budget.add_tasks_used(len(new_items))
        graph = self._graph_getter()
        materialized = [graph[item.id] for item in inserted if item.id in graph]
        for item in materialized:
            self._emit(make_task_added(self._team_run_id, task_to_dict(item)))
        self._budget.emit_update()
        return materialized, True

    async def apply_replan(
        self,
        replan_task_id: str,
        add_tasks: list[TaskDefinition],
        cancel_ids: list[str],
        target_parent_id: str | None,
    ) -> dict[str, int]:
        if replan_task_id in cancel_ids:
            raise InvalidPlan("replanner cannot cancel itself")

        graph = self._graph_getter()
        replanner = graph.get(replan_task_id)
        origin_task_id = replanner.fired_by_task_id if replanner is not None else None
        active_tasks = {
            task_id: task
            for task_id, task in graph.items()
            if task.status not in {TaskStatus.CANCELLED, TaskStatus.DONE, TaskStatus.FAILED}
        }

        def _cascade_ids_for_cancel_root(cancel_root_id: str) -> set[str]:
            children_by_parent: dict[str, list[str]] = {}
            dependents_by_dep: dict[str, list[str]] = {}
            for task_id, task in active_tasks.items():
                if task.parent_id:
                    children_by_parent.setdefault(task.parent_id, []).append(task_id)
                for dep_id in task.deps or []:
                    dependents_by_dep.setdefault(dep_id, []).append(task_id)

            cascaded: set[str] = set()
            queue = [cancel_root_id]
            while queue:
                current = queue.pop(0)
                for child_id in children_by_parent.get(current, []):
                    if child_id not in cascaded:
                        cascaded.add(child_id)
                        queue.append(child_id)
                for dependent_id in dependents_by_dep.get(current, []):
                    dependent = active_tasks.get(dependent_id)
                    if dependent is None:
                        continue
                    if dependent_id not in cascaded:
                        cascaded.add(dependent_id)
                        queue.append(dependent_id)
            cascaded.discard(cancel_root_id)
            return cascaded

        cancelled = set(cancel_ids)
        for cancel_id in cancel_ids:
            cancelled.update(_cascade_ids_for_cancel_root(cancel_id))

        def _is_inside_parent_projection(task_id: str) -> bool:
            task = graph.get(task_id)
            while task is not None:
                if task.parent_id == target_parent_id:
                    return True
                if task.parent_id is None:
                    return target_parent_id is None
                task = graph.get(task.parent_id)
            return False

        allowed_parent_ids: set[str | None] = {target_parent_id, replan_task_id}
        for task in graph.values():
            if task.id in cancelled:
                continue
            if task.id == origin_task_id:
                continue
            if task.status in {TaskStatus.CANCELLED, TaskStatus.DONE, TaskStatus.FAILED}:
                continue
            if _is_inside_parent_projection(task.id):
                allowed_parent_ids.add(task.id)

        for cid in cancel_ids:
            rec = await self._store.get_record(cid)
            if rec is None:
                raise InvalidPlan(f"cancel target {cid} not found")
            if cid == replan_task_id:
                raise InvalidPlan("replanner cannot cancel itself")
            if cid == origin_task_id:
                raise InvalidPlan("replanner cannot cancel the original replanning task")
            if not _is_inside_parent_projection(cid):
                raise InvalidPlan(
                    f"cancel target '{cid}' is outside the allowed parent projection "
                    f"rooted at {target_parent_id!r}"
                )
            if rec.status in {
                status.value
                for status in (TaskStatus.DONE, TaskStatus.FAILED, TaskStatus.CANCELLED)
            }:
                raise InvalidPlan(
                    f"cancel target {cid} is {rec.status}; cannot cancel terminal tasks"
                )

        local_to_new: dict[str, str] = {}
        for spec in add_tasks:
            if spec.parent_id not in allowed_parent_ids:
                raise InvalidPlan(
                    f"new task '{spec.id}' parent_id={spec.parent_id!r} is outside "
                    f"the allowed parent projection rooted at {target_parent_id!r}"
                )
            if spec.parent_id in cancelled:
                raise InvalidPlan(
                    f"new task '{spec.id}' cannot be inserted under cancelled parent "
                    f"'{spec.parent_id}'"
                )
            if spec.id:
                if spec.id in local_to_new:
                    raise InvalidPlan(f"duplicate id '{spec.id}'")
                local_to_new[spec.id] = self.new_id()

        adj = await self._store.get_adjacency()
        clean_adj = {k: v for k, v in adj.items() if k not in cancelled}
        specs: list[TaskDefinition] = []
        for spec in add_tasks:
            nid = local_to_new.get(spec.id, self.new_id()) if spec.id else self.new_id()
            rdeps: list[str] = []
            for d in spec.deps:
                if d in local_to_new:
                    rdeps.append(local_to_new[d])
                elif d in adj and d not in cancelled and d != replan_task_id:
                    rdeps.append(d)
                else:
                    raise InvalidPlan(f"replan dep '{d}' is not a local alias or existing task id")
            clean_adj[nid] = rdeps
            specs.append(
                TaskDefinition(
                    id=nid,
                    objective=spec.objective,
                    agent=spec.agent,
                    description=spec.description or "",
                    deps=rdeps,
                    scope_paths=list(spec.scope_paths),
                    parent_id=spec.parent_id,
                )
            )

        if _has_cycle(clean_adj):
            raise InvalidPlan("replan would create a cycle")

        if not self._budget.has_capacity_for(len(specs)):
            raise BudgetExceeded("max_tasks would be exceeded by replan")

        if self._cancel_active_task is not None:
            for cancelled_id in sorted(cancelled):
                task = graph.get(cancelled_id)
                if task is not None and task.status == TaskStatus.RUNNING:
                    self._cancel_active_task(cancelled_id)

        _, inserted = await self._store.apply_replan_atomic(
            cancel_ids=cancel_ids,
            cancel_reason=f"cancelled_by_replan_{replan_task_id}",
            specs=specs,
        )

        if specs:
            self._budget.charge_tasks(len(specs))
            graph = self._graph_getter()
            for item in inserted:
                if item.id in graph:
                    self._emit(make_task_added(self._team_run_id, task_to_dict(graph[item.id])))

        return {
            "added": len(specs),
            "cancelled": len(cancel_ids),
            "inserted_ids": [r.id for r in inserted],
            "replanner_child_count": sum(1 for spec in specs if spec.parent_id == replan_task_id),
        }
