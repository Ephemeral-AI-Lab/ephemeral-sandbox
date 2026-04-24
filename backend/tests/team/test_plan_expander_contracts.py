"""Contract tests for PlanExpander + the replan validation rules.

PlanExpander now produces a ``GraphMutation`` via ``TaskGraph``; it no longer
touches persistence. These tests assert on the returned ``*Outcome`` object
and the resulting in-memory graph state.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from agents.registry import get_definition
from team.core.errors import InvalidPlan
from team.core.models import (
    BudgetConfig,
    Plan,
    TERMINAL_STATUSES,
    Task,
    TaskDefinition,
    TaskStatus,
)
from team.definitions import register_all as register_team_builtins
from team.planning.expander import PlanExpander, ReplanApplyOutcome
from team.planning.replan_validation import ALLOWED_REPLAN_DEP_STATUSES, validate_replan_rules
from team.runtime.task_graph import TaskGraph
from .helpers import make_task as _task
from .helpers import structured_spec as _spec


if get_definition("developer") is None:
    register_team_builtins()


class _Budget:
    def __init__(self) -> None:
        self.charged = 0
        self.added = 0
        self.budgets = BudgetConfig()

    def has_capacity_for(self, count: int) -> bool:
        del count
        return True

    def charge_tasks(self, count: int) -> None:
        self.charged += count

    def add_tasks_used(self, count: int) -> None:
        self.added += count

    def within_depth_limit(self, new_depth: int) -> bool:
        return new_depth <= self.budgets.max_depth

    def emit_update(self) -> None:
        return None


def _make_expander(
    graph_map: dict[str, Task], *, budget: _Budget | None = None
) -> tuple[PlanExpander, TaskGraph, _Budget]:
    graph = TaskGraph(graph_map)
    b = budget or _Budget()
    expander = PlanExpander(graph=graph, budget=b)
    return expander, graph, b


def test_expander_submitted_root_empty_plan_raises_invalid_plan():
    planner = _task("root", agent_name="team_planner", status=TaskStatus.RUNNING)
    expander, _, _ = _make_expander({"root": planner})

    with pytest.raises(InvalidPlan, match="plan has no tasks"):
        expander.expand_submitted_plan(planner, Plan(tasks=[]))


def test_expander_submitted_nested_empty_plan_raises_invalid_plan():
    planner = _task("child", agent_name="team_planner", status=TaskStatus.RUNNING)
    expander, _, _ = _make_expander({"child": planner})

    with pytest.raises(InvalidPlan, match="plan has no tasks"):
        expander.expand_submitted_plan(planner, Plan(tasks=[]))


def test_replan_empty_apply_returns_outcome_and_nested_call_raises():
    failed = _task("failed", status=TaskStatus.REQUEST_REPLAN)
    replanner = _task(
        "replanner",
        agent_name="team_replanner",
        status=TaskStatus.RUNNING,
        fired_by_task_id="failed",
    )
    expander, _, _ = _make_expander({"failed": failed, "replanner": replanner})

    outcome = expander.apply_replan(
        replan_task=replanner, add_tasks=[], cancel_ids=[]
    )
    assert isinstance(outcome, ReplanApplyOutcome)
    assert outcome.new_tasks == ()
    assert outcome.cancelled_ids == ()
    assert outcome.replanner_child_count == 0

    with pytest.raises(InvalidPlan, match="must be direct children of the replanner"):
        expander.apply_replan(
            replan_task=replanner,
            add_tasks=[
                TaskDefinition(
                    id="bad-child",
                    spec=_spec("Invalid repair under original failed task."),
                    agent="developer",
                    scope_paths=["src/repair.py"],
                    parent_id="failed",
                )
            ],
            cancel_ids=[],
        )


def test_replan_cancel_running_target_reports_in_outcome():
    replanner = _task(
        "replanner", agent_name="team_replanner", status=TaskStatus.RUNNING
    )
    running_target = _task("running-target", status=TaskStatus.RUNNING)
    expander, _, _ = _make_expander({"replanner": replanner, "running-target": running_target})

    outcome = expander.apply_replan(
        replan_task=replanner, add_tasks=[], cancel_ids=["running-target"]
    )

    assert "running-target" in outcome.cancelled_running_ids
    assert "running-target" in outcome.cancelled_ids


def test_replan_cancel_cascades_to_reviewer_dependents():
    replanner = _task(
        "replanner", agent_name="team_replanner", status=TaskStatus.RUNNING
    )
    running_target = _task("running-target", status=TaskStatus.RUNNING)
    reviewer_dep = _task(
        "reviewer-dependent",
        agent_name="reviewer",
        status=TaskStatus.RUNNING,
        deps=["running-target"],
    )
    expander, _, _ = _make_expander({
        "replanner": replanner,
        "running-target": running_target,
        "reviewer-dependent": reviewer_dep,
    })

    outcome = expander.apply_replan(
        replan_task=replanner, add_tasks=[], cancel_ids=["running-target"]
    )

    assert "running-target" in outcome.cancelled_ids
    assert "reviewer-dependent" in outcome.cancelled_ids
    assert "running-target" in outcome.cancelled_running_ids
    assert "reviewer-dependent" in outcome.cancelled_running_ids


def test_replan_rejects_cancellation_of_original_task():
    failed = _task("failed", status=TaskStatus.REQUEST_REPLAN)
    replanner = _task(
        "replanner",
        agent_name="team_replanner",
        status=TaskStatus.RUNNING,
        fired_by_task_id="failed",
    )
    expander, _, _ = _make_expander({"failed": failed, "replanner": replanner})

    with pytest.raises(InvalidPlan, match="original request_replan task"):
        expander.apply_replan(
            replan_task=replanner, add_tasks=[], cancel_ids=["failed"]
        )


def test_replan_allows_children_at_replanner_depth_limit():
    failed = _task("failed", status=TaskStatus.REQUEST_REPLAN, parent_id="parent")
    replanner = _task(
        "replanner",
        agent_name="team_replanner",
        status=TaskStatus.RUNNING,
        parent_id="parent",
        fired_by_task_id="failed",
    )
    budget = _Budget()
    budget.budgets = BudgetConfig(max_depth=1)
    expander, graph, _ = _make_expander(
        {"failed": failed, "replanner": replanner}, budget=budget
    )

    outcome = expander.apply_replan(
        replan_task=replanner,
        add_tasks=[
            TaskDefinition(
                id="same-depth-repair",
                spec=_spec("Repair at the replanner depth limit."),
                agent="developer",
                scope_paths=["src/a.py"],
                parent_id="replanner",
            )
        ],
        cancel_ids=[],
    )

    assert len(outcome.new_tasks) == 1
    assert budget.charged == 1
    assert outcome.replanner_child_count == 1


def test_replan_rejects_insertion_under_original_task():
    failed = _task("failed", status=TaskStatus.REQUEST_REPLAN)
    replanner = _task(
        "replanner",
        agent_name="team_replanner",
        status=TaskStatus.RUNNING,
        fired_by_task_id="failed",
    )
    expander, _, _ = _make_expander({"failed": failed, "replanner": replanner})

    with pytest.raises(InvalidPlan, match="must be direct children of the replanner"):
        expander.apply_replan(
            replan_task=replanner,
            add_tasks=[
                TaskDefinition(
                    id="bad-child",
                    spec=_spec("Invalid repair under original failed task."),
                    agent="developer",
                    scope_paths=["src/repair.py"],
                    parent_id="failed",
                )
            ],
            cancel_ids=[],
        )


def test_replan_applies_plan_policy_to_added_tasks():
    failed = _task("failed", status=TaskStatus.REQUEST_REPLAN)
    replanner = _task(
        "replanner",
        agent_name="team_replanner",
        status=TaskStatus.RUNNING,
        fired_by_task_id="failed",
    )
    expander, _, _ = _make_expander({"failed": failed, "replanner": replanner})

    with pytest.raises(InvalidPlan, match="submitted plans cannot include replanner agent"):
        expander.apply_replan(
            replan_task=replanner,
            add_tasks=[
                TaskDefinition(
                    id="bad-replanner",
                    spec=_spec("Invalid replanner target."),
                    agent="team_replanner",
                    scope_paths=["src/a.py"],
                    parent_id="replanner",
                )
            ],
            cancel_ids=[],
        )


def test_replan_rejects_dep_on_rewired_downstream_task():
    failed = _task("failed", status=TaskStatus.REQUEST_REPLAN)
    replanner = _task(
        "replanner",
        agent_name="team_replanner",
        status=TaskStatus.RUNNING,
        fired_by_task_id="failed",
    )
    downstream = _task("downstream", status=TaskStatus.PENDING, deps=["replanner"])
    expander, _, _ = _make_expander({
        "failed": failed,
        "replanner": replanner,
        "downstream": downstream,
    })

    with pytest.raises(InvalidPlan, match="replan dep 'downstream'"):
        expander.apply_replan(
            replan_task=replanner,
            add_tasks=[
                TaskDefinition(
                    id="repair",
                    spec=_spec("Invalidly wait for downstream work blocked on R."),
                    agent="developer",
                    deps=["downstream"],
                    scope_paths=["src/a.py"],
                    parent_id="replanner",
                )
            ],
            cancel_ids=[],
        )


# ---------------------------------------------------------------------------
# Property tests — validate_replan_rules cascade + allowed-deps logic
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ReplanGraphCase:
    graph: dict[str, Task]
    cancel_root_id: str


_ACTIVE_CANCEL_TARGET_STATUSES = (
    TaskStatus.PENDING,
    TaskStatus.READY,
    TaskStatus.RUNNING,
    TaskStatus.EXPANDED,
)
_GENERATED_STATUSES = (
    TaskStatus.PENDING,
    TaskStatus.READY,
    TaskStatus.RUNNING,
    TaskStatus.EXPANDED,
    TaskStatus.DONE,
    TaskStatus.FAILED,
    TaskStatus.CANCELLED,
)


@st.composite
def _replan_graph_cases(draw: st.DrawFn) -> ReplanGraphCase:
    generated_ids = [f"t{i}" for i in range(draw(st.integers(min_value=0, max_value=7)))]
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
        "cancel-root": _task(
            "cancel-root",
            status=draw(st.sampled_from(_ACTIVE_CANCEL_TARGET_STATUSES)),
        ),
    }

    for index, task_id in enumerate(generated_ids):
        prior_ids = generated_ids[:index]
        parent_id = draw(st.sampled_from([None, "parent", "cancel-root", *prior_ids]))
        dep_choices = ["failed", "replanner", "cancel-root", *prior_ids]
        deps = draw(st.lists(st.sampled_from(dep_choices), unique=True, max_size=3))
        graph[task_id] = _task(
            task_id,
            status=draw(st.sampled_from(_GENERATED_STATUSES)),
            parent_id=parent_id,
            deps=deps,
        )

    return ReplanGraphCase(graph=graph, cancel_root_id="cancel-root")


def _active_ids(graph: dict[str, Task]) -> set[str]:
    return {task_id for task_id, task in graph.items() if task.status not in TERMINAL_STATUSES}


def _reference_cascade_ids(
    graph: dict[str, Task],
    cancel_root_ids: Iterable[str],
) -> set[str]:
    active = _active_ids(graph)
    cascaded = set(cancel_root_ids)
    queue = list(cancel_root_ids)
    while queue:
        current = queue.pop(0)
        for task_id, task in graph.items():
            if task_id not in active or task_id in cascaded:
                continue
            if task.parent_id == current or current in task.deps:
                cascaded.add(task_id)
                queue.append(task_id)
    return cascaded


def _reference_depends_on_any(
    graph: dict[str, Task],
    *,
    task_id: str,
    blocked_dep_ids: set[str],
) -> bool:
    task = graph[task_id]
    stack = list(task.deps)
    seen: set[str] = set()
    while stack:
        dep_id = stack.pop()
        if dep_id in blocked_dep_ids:
            return True
        if dep_id in seen:
            continue
        seen.add(dep_id)
        dep_task = graph.get(dep_id)
        if dep_task is not None:
            stack.extend(dep_task.deps)
    return False


def _reference_allowed_existing_dep_ids(
    graph: dict[str, Task],
    *,
    all_cancelled_ids: set[str],
) -> set[str]:
    excluded = {"failed", "replanner"}
    allowed: set[str] = set()
    for task_id, task in graph.items():
        if task_id in all_cancelled_ids or task_id in excluded:
            continue
        if _reference_depends_on_any(graph, task_id=task_id, blocked_dep_ids=excluded):
            continue
        if task.status.value in ALLOWED_REPLAN_DEP_STATUSES:
            allowed.add(task_id)
    return allowed


@given(_replan_graph_cases())
@settings(max_examples=120, deadline=None)
def test_replan_validation_cancel_cascade_matches_generated_graph(
    case: ReplanGraphCase,
) -> None:
    result = validate_replan_rules(
        graph=case.graph,
        replan_task_id="replanner",
        cancel_ids=[case.cancel_root_id],
    )

    assert result.errors == []
    assert result.all_cancelled_ids == _reference_cascade_ids(
        case.graph,
        [case.cancel_root_id],
    )


@given(_replan_graph_cases())
@settings(max_examples=120, deadline=None)
def test_replan_validation_allowed_existing_deps_match_generated_graph(
    case: ReplanGraphCase,
) -> None:
    result = validate_replan_rules(
        graph=case.graph,
        replan_task_id="replanner",
        cancel_ids=[case.cancel_root_id],
    )

    assert result.errors == []
    assert result.allowed_existing_dep_ids == _reference_allowed_existing_dep_ids(
        case.graph,
        all_cancelled_ids=result.all_cancelled_ids,
    )
