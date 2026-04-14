"""PlanExpander — submitted-plan validation, ID remapping, replan application.

Extracted from TaskCenter. Owns:
- Validating submitted plans (depth, size, cycles, budget)
- Remapping local plan IDs to global UUIDs
- Inserting expanded children into the task graph
- Applying replans (cancel siblings + add new tasks)

Validation failures during plan expansion cascade-cancel the parent via
``cascade_fail_cb`` and return ``ok=False``; they do not raise. Replan
validation failures (apply_replan) raise InvalidPlan/BudgetExceeded so the
caller can surface them to the requester.
"""

from __future__ import annotations

import logging
import uuid
from typing import Any, Awaitable, Callable

from team.budget_manager import BudgetManager
from team.errors import BudgetExceeded, InvalidPlan
from team.models import AgentResult, Task, TaskSpec, TaskStatus
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
    ) -> None:
        self._team_run_id = team_run_id
        self._store = store
        self._budget = budget
        self._graph_getter = graph_getter
        self._emit = emit_cb
        self._cascade_fail = cascade_fail_cb

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
        from agents.registry import has_role as _has_role

        task_id = rec.id

        if _has_role(rec.agent_name, "planner") and result.submitted_plan is None:
            await self._cascade_fail(
                task_id, "InvalidPlan: expandable task did not submit a plan"
            )
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
            await self._cascade_fail(
                task_id, "InvalidPlan: " + "; ".join(i["msg"] for i in issues)
            )
            return [], False

        local_to_global: dict[str, str] = {
            spec.id: self.new_id() for spec in result.submitted_plan.tasks if spec.id
        }
        specs: list[TaskSpec] = []
        new_items: list[Task] = []
        for spec in result.submitted_plan.tasks:
            nid = local_to_global.get(spec.id) or self.new_id()
            rdeps = [local_to_global[d] if d in local_to_global else d for d in spec.deps]
            specs.append(
                TaskSpec(
                    id=nid,
                    task=spec.task,
                    agent=spec.agent,
                    deps=rdeps,
                    scope_paths=list(spec.scope_paths),
                    cascade_policy=spec.cascade_policy,
                )
            )
            new_items.append(
                Task(
                    id=nid,
                    team_run_id=self._team_run_id,
                    agent_name=spec.agent,
                    status=TaskStatus.READY if not rdeps else TaskStatus.PENDING,
                    task=spec.task,
                    deps=rdeps,
                    scope_paths=list(spec.scope_paths),
                    cascade_policy=spec.cascade_policy,
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
        add_tasks: list[TaskSpec],
        cancel_ids: list[str],
        target_depth: int,
        target_parent_id: str | None,
        target_root_id: str,
    ) -> dict[str, int]:
        for cid in cancel_ids:
            rec = await self._store.get_record(cid)
            if rec is None:
                raise InvalidPlan(f"cancel target {cid} not found")
            if rec.parent_id != target_parent_id:
                raise InvalidPlan(
                    f"cancel target '{cid}' is a child of '{rec.parent_id}', not a sibling at your level. "
                    f"You can only cancel siblings (tasks with parent_id={target_parent_id!r}). "
                    f"To cancel '{cid}' and its entire subtree, cancel its parent '{rec.parent_id}' instead."
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

        adj = await self._store.get_adjacency()
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
                    raise InvalidPlan(
                        f"replan dep '{d}' is not a local alias or existing task id"
                    )
            clean_adj[nid] = rdeps
            specs.append(
                TaskSpec(
                    id=nid,
                    task=spec.task,
                    agent=spec.agent,
                    deps=rdeps,
                    scope_paths=list(spec.scope_paths),
                    cascade_policy=spec.cascade_policy,
                )
            )

        if _has_cycle(clean_adj):
            raise InvalidPlan("replan would create a cycle")

        if not self._budget.has_capacity_for(len(specs)):
            raise BudgetExceeded("max_tasks would be exceeded by replan")

        _, inserted = await self._store.apply_replan_atomic(
            cancel_ids=cancel_ids,
            cancel_reason=f"cancelled_by_replan_{replan_task_id}",
            specs=specs,
            parent_id=target_parent_id,
            parent_depth=max(0, target_depth - 1),
            parent_root_id=target_root_id or None,
        )

        if specs:
            self._budget.charge_tasks(len(specs))
            graph = self._graph_getter()
            for item in inserted:
                if item.id in graph:
                    self._emit(
                        make_task_added(self._team_run_id, task_to_dict(graph[item.id]))
                    )

        return {"added": len(specs), "cancelled": len(cancel_ids), "inserted_ids": [r.id for r in inserted]}
