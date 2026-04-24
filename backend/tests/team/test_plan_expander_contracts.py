from __future__ import annotations

from dataclasses import dataclass
from types import SimpleNamespace
from typing import Iterable

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from agents.registry import get_definition
from team.definitions import register_all as register_team_builtins
from team.core.errors import InvalidPlan
from team.core.models import (
    BudgetConfig,
    Plan,
    TERMINAL_STATUSES,
    Task,
    TaskDefinition,
    TaskStatus,
)
from .helpers import make_task as _task
from .helpers import structured_spec as _spec
from team.planning.expander import PlanExpander, ReplanApplyOutcome
from team.planning.replan_validation import ALLOWED_REPLAN_DEP_STATUSES, validate_replan_rules


if get_definition("developer") is None:
    register_team_builtins()


class _ExpanderStore:
    def __init__(self, graph: dict[str, Task]) -> None:
        self.graph = graph
        self.calls: list[str] = []

    async def get_adjacency(self) -> dict[str, list[str]]:
        return {task_id: list(task.deps) for task_id, task in self.graph.items()}

    async def insert_plan(
        self,
        specs: list[TaskDefinition],
        **kwargs: object,
    ) -> list[TaskDefinition]:
        del specs, kwargs
        self.calls.append("insert_plan")
        return []

    async def apply_replan_atomic(
        self,
        **kwargs: object,
    ) -> tuple[int, list[TaskDefinition]]:
        self.calls.append("apply_replan_atomic")
        cancel_ids = kwargs["cancel_ids"]
        assert isinstance(cancel_ids, list)
        return len(cancel_ids), []


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


def _ignore_event(event: object) -> None:
    del event


@pytest.mark.asyncio
async def test_expander_submitted_root_empty_plan_fails_via_outcome_not_raise():
    graph = {"root": _task("root", agent_name="team_planner", status=TaskStatus.RUNNING)}
    store = _ExpanderStore(graph)

    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
    )

    with pytest.raises(InvalidPlan, match="plan has no tasks"):
        await expander.expand_submitted_plan(
            SimpleNamespace(id="root", agent_name="team_planner", depth=0, root_id="root"),
            Plan(tasks=[]),
        )

    assert store.calls == []


@pytest.mark.asyncio
async def test_expander_submitted_nested_empty_plan_is_rejected_as_typed_outcome():
    graph = {"child": _task("child", agent_name="team_planner", status=TaskStatus.RUNNING)}
    store = _ExpanderStore(graph)

    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
    )

    with pytest.raises(InvalidPlan, match="plan has no tasks"):
        await expander.expand_submitted_plan(
            SimpleNamespace(id="child", agent_name="team_planner", depth=1, root_id="root"),
            Plan(tasks=[]),
        )


@pytest.mark.asyncio
async def test_expander_replan_returns_typed_outcome_and_raises_invalid_plan():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }

    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=lambda event: None,
    )

    outcome = await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[],
        cancel_ids=[],
    )

    assert isinstance(outcome, ReplanApplyOutcome)
    assert outcome.added == 0
    assert len(outcome.cancelled_ids) == 0
    assert outcome.inserted_ids == ()
    assert outcome.replanner_child_count == 0

    with pytest.raises(InvalidPlan, match="must be direct children of the replanner"):
        await expander.apply_replan(
            replan_task_id="replanner",
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


@pytest.mark.asyncio
async def test_replan_cancels_active_runner_after_apply_replan_atomic_commits():
    graph = {
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
        ),
        "running-target": _task("running-target", status=TaskStatus.RUNNING),
    }
    store = _ExpanderStore(graph)
    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,
    )

    outcome = await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[],
        cancel_ids=["running-target"],
    )

    # DB transaction commits BEFORE runtime cancellation so a rollback
    # cannot leave the graph saying the task is RUNNING while its runner
    # has already been killed.
    assert store.calls == ["apply_replan_atomic"]
    assert "running-target" in outcome.cancelled_running_ids


@pytest.mark.asyncio
async def test_replan_cancel_cascade_includes_reviewer_dependents():
    graph = {
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
        ),
        "running-target": _task("running-target", status=TaskStatus.RUNNING),
        "reviewer-dependent": _task(
            "reviewer-dependent",
            agent_name="reviewer",
            status=TaskStatus.RUNNING,
            deps=["running-target"],
        ),
    }
    store = _ExpanderStore(graph)
    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,
    )

    outcome = await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[],
        cancel_ids=["running-target"],
    )

    assert store.calls == ["apply_replan_atomic"]
    assert "running-target" in outcome.cancelled_running_ids
    assert "reviewer-dependent" in outcome.cancelled_running_ids


@pytest.mark.asyncio
async def test_replan_does_not_cancel_runner_when_apply_replan_atomic_raises():
    """If apply_replan_atomic fails, no live runner cancellation happens,
    so graph state and runner state stay consistent under rollback."""

    class _RaisingStore(_ExpanderStore):
        async def apply_replan_atomic(
            self,
            **kwargs: object,
        ) -> tuple[int, list[TaskDefinition]]:
            del kwargs
            self.calls.append("apply_replan_atomic")
            raise RuntimeError("db commit failed")

    graph = {
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
        ),
        "running-target": _task("running-target", status=TaskStatus.RUNNING),
    }
    store = _RaisingStore(graph)
    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,
    )

    with pytest.raises(RuntimeError, match="db commit failed"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[],
            cancel_ids=["running-target"],
        )

    assert store.calls == ["apply_replan_atomic"]


@pytest.mark.asyncio
async def test_replan_expander_rejects_original_task_cancellation():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,

    )

    with pytest.raises(InvalidPlan, match="original request_replan task"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[],
            cancel_ids=["failed"],
        )


@pytest.mark.asyncio
async def test_replan_expander_allows_children_at_replanner_depth_limit():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN, parent_id="parent"),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            parent_id="parent",
            fired_by_task_id="failed",
        ),
    }
    store = _ExpanderStore(graph)
    budget = _Budget()
    budget.budgets = BudgetConfig(max_depth=1)
    expander = PlanExpander(
        team_run_id="run-1",
        store=store,
        budget=budget,
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,

    )

    outcome = await expander.apply_replan(
        replan_task_id="replanner",
        add_tasks=[
            TaskDefinition(
                id="same-depth-repair",
                spec=_spec("Repair at the replanner depth limit."),
                agent="developer",
                description="repair at depth limit",
                scope_paths=["src/a.py"],
                parent_id="replanner",
            )
        ],
        cancel_ids=[],
    )

    assert outcome.added == 1
    assert budget.charged == 1
    assert store.calls == ["apply_replan_atomic"]


@pytest.mark.asyncio
async def test_replan_expander_rejects_insertion_under_original_task():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,

    )

    with pytest.raises(InvalidPlan, match="must be direct children of the replanner"):
        await expander.apply_replan(
            replan_task_id="replanner",
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


@pytest.mark.asyncio
async def test_replan_expander_applies_plan_policy_to_added_tasks():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,

    )

    with pytest.raises(InvalidPlan, match="submitted plans cannot include replanner agent"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[
                TaskDefinition(
                    id="bad-replanner",
                    spec=_spec("Invalid replanner target."),
                    agent="team_replanner",
                    description="invalid replanner target",
                    scope_paths=["src/a.py"],
                    parent_id="replanner",
                )
            ],
            cancel_ids=[],
        )


@pytest.mark.asyncio
async def test_replan_expander_rejects_dep_on_rewired_downstream_task():
    graph = {
        "failed": _task("failed", status=TaskStatus.REQUEST_REPLAN),
        "replanner": _task(
            "replanner",
            agent_name="team_replanner",
            status=TaskStatus.RUNNING,
            fired_by_task_id="failed",
        ),
        "downstream": _task(
            "downstream",
            status=TaskStatus.PENDING,
            deps=["replanner"],
        ),
    }
    expander = PlanExpander(
        team_run_id="run-1",
        store=_ExpanderStore(graph),
        budget=_Budget(),
        graph_getter=lambda: graph,
        emit_cb=_ignore_event,

    )

    with pytest.raises(InvalidPlan, match="replan dep 'downstream'"):
        await expander.apply_replan(
            replan_task_id="replanner",
            add_tasks=[
                TaskDefinition(
                    id="repair",
                    spec=_spec("Invalidly wait for downstream work blocked on R."),
                    agent="developer",
                    description="invalid downstream dependency",
                    deps=["downstream"],
                    scope_paths=["src/a.py"],
                    parent_id="replanner",
                )
            ],
            cancel_ids=[],
        )


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
