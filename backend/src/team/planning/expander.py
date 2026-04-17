"""PlanExpander — submitted-plan validation, ID remapping, replan application.

Extracted from TaskCenter. Owns:
- Validating submitted plans (depth, size, cycles, budget)
- Remapping local plan IDs to global UUIDs
- Inserting expanded children into the task graph
- Applying replans (cancel allowed graph-region tasks + add new tasks)

Validation failures during plan expansion fail the parent (a still-RUNNING
leaf) via ``fail_cb`` and return ``ok=False``; they do not raise. Replan
validation failures (apply_replan) raise InvalidPlan so the caller can
surface them to the requester. BudgetExceeded (max_tasks) is a team-run
level guarantee — it always raises and is handled by the executor via
``fail_fast``, never locally.
"""

from __future__ import annotations

import uuid
from typing import Awaitable, Callable

from agents.registry import has_role
from team.budget_manager import BudgetManager
from team.errors import BudgetExceeded, InvalidPlan
from team.models import AgentResult, Plan, Task, TaskDefinition, TaskStatus
from team.persistence.events import TeamRunEvent, make_task_added, task_to_dict
from team.persistence.task_record import TaskRecord
from team.persistence.task_store import TaskStore
from team.planning.replan_validation import validate_replan_rules
from team.planning.validation import _has_cycle, validate_plan


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
        fail_cb: Callable[[str, str], Awaitable[None]],
        cancel_running_task_cb: Callable[[str], None] | None = None,
    ) -> None:
        self._team_run_id = team_run_id
        self._store = store
        self._budget = budget
        self._graph_getter = graph_getter
        self._emit = emit_cb
        self._fail = fail_cb
        self._cancel_running_task = cancel_running_task_cb

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
            await self._fail(task_id, "InvalidPlan: expandable task did not submit a plan")
            return [], False

        if result.submitted_plan is None:
            return [], True

        new_depth = (rec.depth or 0) + 1
        if not self._budget.within_depth_limit(new_depth):
            await self._fail(
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
            await self._fail(task_id, "InvalidPlan: " + "; ".join(i["msg"] for i in issues))
            return [], False

        local_to_global: dict[str, str] = {
            spec.id: self.new_id() for spec in result.submitted_plan.tasks if spec.id
        }
        specs: list[TaskDefinition] = []
        new_items: list[Task] = []
        for spec in result.submitted_plan.tasks:
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
        return materialized, True

    async def apply_replan(
        self,
        replan_task_id: str,
        add_tasks: list[TaskDefinition],
        cancel_ids: list[str],
    ) -> dict[str, int | list[str]]:
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
            new_depth = (getattr(replanner, "depth", 0) or 0) + 1
            if not self._budget.within_depth_limit(new_depth):
                raise InvalidPlan(
                    f"replan would exceed max_depth={self._budget.budgets.max_depth} "
                    f"from current depth={getattr(replanner, 'depth', 0) or 0}"
                )

        plan_issues = validate_plan(
            Plan(tasks=add_tasks),
            max_plan_size=self._budget.budgets.max_plan_size,
            allow_empty=True,
            known_external_deps=allowed_existing_dep_ids,
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

        # Commit the graph mutation FIRST. If apply_replan_atomic raises,
        # no live runner cancellation has happened yet — state stays consistent
        # (graph still says task is RUNNING, runner is still alive). Cancelling
        # before commit risks killing runners while the DB rolls back.
        _, inserted = await self._store.apply_replan_atomic(
            cancel_ids=cancel_ids,
            cancel_reason=f"cancelled_by_replan_{replan_task_id}",
            specs=specs,
        )

        if self._cancel_running_task is not None:
            for cancelled_id in sorted(cancelled):
                task = graph.get(cancelled_id)
                if task is not None and task.status == TaskStatus.RUNNING:
                    self._cancel_running_task(cancelled_id)

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
