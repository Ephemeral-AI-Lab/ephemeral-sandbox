"""PlanExpander — validate submitted plans/replans, return ``GraphMutation``.

Owns only planning concerns: structural validation, local→global ID remap,
budget checks, replan rule validation. The actual graph mutation is built
by :class:`team.runtime.task_graph.TaskGraph`; the coordinator persists and
applies it.

Validation failures raise ``InvalidPlan``. Budget overruns raise
``BudgetExceeded``. Callers (``TaskCoordinator``) translate either into a
``FAILED`` update on the submitting task.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING

from agents.registry import has_role
from team.core.errors import BudgetExceeded, InvalidPlan
from team.core.models import Plan, Task, TaskDefinition
from team.runtime.task_graph import GraphMutation, TaskGraph
from team.planning.replan_validation import validate_replan_rules
from team.planning.validation import _has_cycle, validate_plan

if TYPE_CHECKING:
    from team.task_center.budget import BudgetManager


@dataclass(frozen=True)
class PlanExpansionOutcome:
    """Result of ``expand_submitted_plan`` — the mutation to apply + the
    new in-memory ``Task`` objects (for event emission and enqueue)."""

    mutation: GraphMutation
    new_tasks: tuple[Task, ...] = ()


@dataclass(frozen=True)
class ReplanApplyOutcome:
    """Result of ``apply_replan``."""

    mutation: GraphMutation
    new_tasks: tuple[Task, ...] = ()
    cancelled_ids: tuple[str, ...] = ()
    cancelled_running_ids: tuple[str, ...] = ()
    replanner_child_count: int = 0


class PlanExpander:
    """Validates plans/replans and produces ``GraphMutation`` via ``TaskGraph``."""

    def __init__(
        self,
        *,
        graph: TaskGraph,
        budget: "BudgetManager",
    ) -> None:
        self._graph = graph
        self._budget = budget

    @staticmethod
    def new_id() -> str:
        return str(uuid.uuid4())

    def expand_submitted_plan(
        self,
        task: Task,
        plan: Plan | None,
    ) -> PlanExpansionOutcome:
        """Validate and build inserts for a submitted plan.

        Raises ``InvalidPlan`` when the submission fails validation (planner
        submitted nothing, depth overrun, cycles, invalid deps). Raises
        ``BudgetExceeded`` when the plan would push ``max_tasks`` over.
        """
        if has_role(task.agent, "planner") and plan is None:
            raise InvalidPlan("expandable task did not submit a plan")

        if plan is None:
            return PlanExpansionOutcome(mutation=GraphMutation.empty())

        new_depth = (task.depth or 0) + 1
        if not self._budget.within_depth_limit(new_depth):
            raise InvalidPlan(
                f"plan would exceed max_depth={self._budget.budgets.max_depth} "
                f"(current depth={task.depth or 0}). Planners at the depth limit must "
                f"emit developer tasks with broader scopes instead of nested "
                f"team_planner tasks."
            )

        issues = validate_plan(
            plan,
            max_plan_size=self._budget.budgets.max_plan_size,
        )
        if issues:
            raise InvalidPlan("; ".join(i["msg"] for i in issues))

        local_to_global: dict[str, str] = {
            spec.id: self.new_id() for spec in plan.tasks if spec.id
        }
        specs: list[TaskDefinition] = []
        for spec in plan.tasks:
            new_task_id = local_to_global.get(spec.id) or self.new_id()
            resolved_deps = [
                local_to_global[dep_id] if dep_id in local_to_global else dep_id
                for dep_id in spec.deps
            ]
            specs.append(
                TaskDefinition(
                    id=new_task_id,
                    spec=spec.spec,
                    agent=spec.agent,
                    deps=resolved_deps,
                    scope_paths=list(spec.scope_paths),
                )
            )

        if not self._budget.has_capacity_for(len(specs)):
            raise BudgetExceeded(
                f"max_tasks={self._budget.budgets.max_tasks} would be exceeded by plan"
            )

        mutation = self._graph.insert_plan_children(parent_id=task.id, specs=specs)
        self._budget.add_tasks_used(len(specs))
        new_tasks = tuple(insert.task for insert in mutation.inserts)
        return PlanExpansionOutcome(mutation=mutation, new_tasks=new_tasks)

    def apply_replan(
        self,
        *,
        replan_task: Task,
        add_tasks: list[TaskDefinition],
        cancel_ids: list[str],
    ) -> ReplanApplyOutcome:
        graph_map = self._graph.tasks
        result = validate_replan_rules(
            graph=graph_map,
            replan_task_id=replan_task.id,
            cancel_ids=cancel_ids,
        )
        if result.errors:
            raise InvalidPlan("; ".join(result.errors))

        if add_tasks:
            replan_depth = replan_task.depth or 0
            if not self._budget.within_depth_limit(replan_depth):
                raise InvalidPlan(
                    f"replan would exceed max_depth={self._budget.budgets.max_depth} "
                    f"from current depth={replan_depth}"
                )
            misplaced = [
                spec.id or "<unknown>"
                for spec in add_tasks
                if spec.parent_id not in (None, replan_task.id)
            ]
            if misplaced:
                raise InvalidPlan(
                    "replan add_tasks must be direct children of the replanner; "
                    f"invalid task ids: {', '.join(misplaced)}"
                )

        if add_tasks:
            local_ids = {spec.id for spec in add_tasks if spec.id}
            local_only_tasks = [
                TaskDefinition(
                    id=spec.id,
                    spec=spec.spec,
                    agent=spec.agent,
                    deps=[dep_id for dep_id in spec.deps if dep_id in local_ids],
                    scope_paths=list(spec.scope_paths),
                )
                for spec in add_tasks
            ]
            plan_issues = validate_plan(
                Plan(tasks=local_only_tasks),
                max_plan_size=self._budget.budgets.max_plan_size,
            )
            if plan_issues:
                raise InvalidPlan("; ".join(issue["msg"] for issue in plan_issues))

        local_to_new: dict[str, str] = {}
        for spec in add_tasks:
            if spec.id:
                if spec.id in local_to_new:
                    raise InvalidPlan(f"duplicate id '{spec.id}'")
                local_to_new[spec.id] = self.new_id()

        clean_adjacency = {
            tid: list(t.deps)
            for tid, t in graph_map.items()
            if tid not in result.all_cancelled_ids
        }
        resolved_specs: list[TaskDefinition] = []
        for spec in add_tasks:
            new_task_id = (
                local_to_new.get(spec.id, self.new_id()) if spec.id else self.new_id()
            )
            resolved_deps: list[str] = []
            for dep_id in spec.deps:
                if dep_id in local_to_new:
                    resolved_deps.append(local_to_new[dep_id])
                elif dep_id in result.allowed_existing_dep_ids:
                    resolved_deps.append(dep_id)
                else:
                    raise InvalidPlan(
                        f"replan dep '{dep_id}' is not a local alias or a schedulable existing task"
                    )
            clean_adjacency[new_task_id] = resolved_deps
            resolved_specs.append(
                TaskDefinition(
                    id=new_task_id,
                    spec=spec.spec,
                    agent=spec.agent,
                    deps=resolved_deps,
                    scope_paths=list(spec.scope_paths),
                    parent_id=replan_task.id,
                )
            )

        if _has_cycle(clean_adjacency):
            raise InvalidPlan("replan would create a cycle")

        if not self._budget.has_capacity_for(len(resolved_specs)):
            raise BudgetExceeded("max_tasks would be exceeded by replan")

        apply_result = self._graph.apply_replan(
            replan_task_id=replan_task.id,
            add_tasks=resolved_specs,
            cancel_ids=cancel_ids,
        )

        if resolved_specs:
            self._budget.charge_tasks(len(resolved_specs))

        return ReplanApplyOutcome(
            mutation=apply_result.mutation,
            new_tasks=apply_result.inserted_tasks,
            cancelled_ids=apply_result.cancelled_ids,
            cancelled_running_ids=apply_result.cancelled_running_ids,
            replanner_child_count=apply_result.replanner_child_count,
        )
