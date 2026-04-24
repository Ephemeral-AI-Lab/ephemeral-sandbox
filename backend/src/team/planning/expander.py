"""PlanExpander — submitted-plan validation, ID remapping, replan application.

Extracted from TaskCenter. Owns:
- Validating submitted plans (depth, size, cycles, budget)
- Remapping local plan IDs to global UUIDs
- Inserting expanded children into the task graph
- Applying replans (cancel allowed graph-region tasks + add new tasks)

Validation failures raise ``InvalidPlan``. Budget overruns raise
``BudgetExceeded``. Callers (``TaskStatusHandler``) translate either into a
``FAILED`` update on the submitting task so the run aborts cleanly.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import Callable, Iterable

from agents.registry import has_role
from team.budget_manager import BudgetManager
from team.errors import BudgetExceeded, InvalidPlan
from team.models import Plan, Task, TaskDefinition, TaskStatus
from team.persistence.events import TeamRunEvent, make_task_added, task_to_dict
from team.persistence.task_record import TaskRecord
from team.persistence.task_store import TaskStore
from team.planning.replan_validation import validate_replan_rules
from team.planning.validation import _has_cycle, validate_plan


@dataclass(frozen=True)
class PlanExpansionOutcome:
    """Typed result for submitted-plan expansion."""

    new_items: tuple[Task, ...] = ()

    @classmethod
    def with_items(cls, items: Iterable[Task] = ()) -> PlanExpansionOutcome:
        return cls(new_items=tuple(items))


@dataclass(frozen=True)
class ReplanApplyOutcome:
    """Typed result for a successfully applied runtime replan.

    Invalid replans raise ``InvalidPlan`` before this outcome is constructed.
    """

    added: int
    cancelled_ids: tuple[str, ...] = ()
    cancelled_running_ids: tuple[str, ...] = ()
    inserted_ids: tuple[str, ...] = ()
    replanner_child_count: int = 0


class PlanExpander:
    """Validates and applies submitted plans + replans."""

    def __init__(
        self,
        *,
        team_run_id: str,
        store: TaskStore,
        budget: BudgetManager,
        graph_getter: Callable[[], dict[str, Task]],
        emit_cb: Callable[[TeamRunEvent], None],
    ) -> None:
        self._team_run_id = team_run_id
        self._store = store
        self._budget = budget
        self._graph_getter = graph_getter
        self._emit = emit_cb

    @staticmethod
    def new_id() -> str:
        return str(uuid.uuid4())

    async def expand_submitted_plan(
        self,
        rec: TaskRecord,
        plan: Plan | None,
    ) -> PlanExpansionOutcome:
        """Validate and insert children for a submitted plan.

        Raises ``InvalidPlan`` when the submission fails validation (planner
        submitted nothing, depth overrun, cycles, invalid deps). Raises
        ``BudgetExceeded`` when the plan would push ``max_tasks`` over the
        limit. Callers translate either into a ``FAILED`` status update.
        """
        task_id = rec.id

        if has_role(rec.agent_name, "planner") and plan is None:
            raise InvalidPlan("expandable task did not submit a plan")

        if plan is None:
            return PlanExpansionOutcome.with_items()

        new_depth = (rec.depth or 0) + 1
        if not self._budget.within_depth_limit(new_depth):
            raise InvalidPlan(
                f"plan would exceed max_depth={self._budget.budgets.max_depth} "
                f"(current depth={rec.depth or 0}). Planners at the depth limit must "
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
        new_items: list[Task] = []
        for spec in plan.tasks:
            new_task_id = local_to_global.get(spec.id) or self.new_id()
            resolved_deps = [
                local_to_global[dep_id] if dep_id in local_to_global else dep_id
                for dep_id in spec.deps
            ]
            specs.append(
                TaskDefinition(
                    id=new_task_id,
                    objective=spec.objective,
                    agent=spec.agent,
                    description=spec.description or "",
                    deps=resolved_deps,
                    scope_paths=list(spec.scope_paths),
                )
            )
            new_items.append(
                Task(
                    id=new_task_id,
                    team_run_id=self._team_run_id,
                    agent_name=spec.agent,
                    status=TaskStatus.READY if not resolved_deps else TaskStatus.PENDING,
                    objective=spec.objective,
                    description=spec.description or "",
                    deps=resolved_deps,
                    scope_paths=list(spec.scope_paths),
                    parent_id=task_id,
                    root_id=rec.root_id or task_id,
                    depth=new_depth,
                )
            )

        if not self._budget.has_capacity_for(len(new_items)):
            raise BudgetExceeded(
                f"max_tasks={self._budget.budgets.max_tasks} would be exceeded by plan"
            )

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
        return PlanExpansionOutcome.with_items(materialized)

    async def apply_replan(
        self,
        replan_task_id: str,
        add_tasks: list[TaskDefinition],
        cancel_ids: list[str],
    ) -> ReplanApplyOutcome:
        graph = self._graph_getter()
        replanner = graph.get(replan_task_id)
        misplaced = [
            spec
            for spec in add_tasks
            if spec.parent_id != replan_task_id
        ]
        if misplaced:
            raise InvalidPlan(
                "replan add_tasks must be direct children of the replanner "
                f"(parent_id={replan_task_id!r}); found "
                + ", ".join(
                    f"'{s.id}' parent_id={s.parent_id!r}" for s in misplaced
                )
            )

        result = validate_replan_rules(
            graph=graph,
            replan_task_id=replan_task_id,
            cancel_ids=cancel_ids,
        )
        if result.errors:
            raise InvalidPlan("; ".join(result.errors))

        cancelled = result.all_cancelled_ids
        allowed_existing_dep_ids = result.allowed_existing_dep_ids

        if add_tasks:
            replan_depth = getattr(replanner, "depth", 0) or 0
            if not self._budget.within_depth_limit(replan_depth):
                raise InvalidPlan(
                    f"replan would exceed max_depth={self._budget.budgets.max_depth} "
                    f"from current depth={replan_depth}"
                )

        plan_issues: list[dict[str, str]] = []
        if add_tasks:
            local_ids = {spec.id for spec in add_tasks if spec.id}
            local_only_tasks = [
                TaskDefinition(
                    id=spec.id,
                    objective=spec.objective,
                    agent=spec.agent,
                    description=spec.description,
                    deps=[dep_id for dep_id in spec.deps if dep_id in local_ids],
                    scope_paths=list(spec.scope_paths),
                    parent_id=spec.parent_id,
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

        adjacency = await self._store.get_adjacency()
        clean_adjacency = {k: v for k, v in adjacency.items() if k not in cancelled}
        specs: list[TaskDefinition] = []
        for spec in add_tasks:
            new_task_id = local_to_new.get(spec.id, self.new_id()) if spec.id else self.new_id()
            resolved_deps: list[str] = []
            for dep_id in spec.deps:
                if dep_id in local_to_new:
                    resolved_deps.append(local_to_new[dep_id])
                elif dep_id in allowed_existing_dep_ids:
                    resolved_deps.append(dep_id)
                else:
                    raise InvalidPlan(
                        f"replan dep '{dep_id}' is not a local alias or a schedulable existing task"
                    )
            clean_adjacency[new_task_id] = resolved_deps
            specs.append(
                TaskDefinition(
                    id=new_task_id,
                    objective=spec.objective,
                    agent=spec.agent,
                    description=spec.description or "",
                    deps=resolved_deps,
                    scope_paths=list(spec.scope_paths),
                    parent_id=spec.parent_id,
                )
            )

        if _has_cycle(clean_adjacency):
            raise InvalidPlan("replan would create a cycle")

        if not self._budget.has_capacity_for(len(specs)):
            raise BudgetExceeded("max_tasks would be exceeded by replan")

        # Snapshot running IDs from the graph *before* we mutate the DB so
        # the handler can cancel live runner tasks for the tasks this replan
        # is cancelling (post-commit, the DB/graph shows them as CANCELLED).
        cancelled_running_ids = tuple(
            sorted(
                cid
                for cid in cancelled
                if graph.get(cid) is not None
                and graph[cid].status == TaskStatus.RUNNING
            )
        )

        # Commit the graph mutation FIRST. If apply_replan_atomic raises,
        # no live runner cancellation has happened yet — state stays consistent
        # (graph still says task is RUNNING, runner is still alive). Cancelling
        # before commit risks killing runners while the DB rolls back.
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

        return ReplanApplyOutcome(
            added=len(specs),
            cancelled_ids=tuple(sorted(cancelled)),
            cancelled_running_ids=cancelled_running_ids,
            inserted_ids=tuple(r.id for r in inserted),
            replanner_child_count=sum(1 for spec in specs if spec.parent_id == replan_task_id),
        )
