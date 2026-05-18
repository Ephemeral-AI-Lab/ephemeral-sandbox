"""US-018: end-to-end planner capability fork.

Builds a parent request whose harness attempt submitted a partial plan, spawns
a child request, then asserts the planner spawned for the child:

* is the ``planner_full_only`` agent (resolver swapped via the variant);
* selects the full-only agent definition;
* the registered ``planner_full_only`` AgentDefinition has
  ``terminals`` without ``submit_plan_continues_goal`` (the gate is the agent.md
  ``terminals:`` filter — the model never sees the tool when the variant
  fires).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from agents import (
    get_definition,
    list_definitions,
    load_agents_tree,
    register_definition,
    unregister_definition,
)
from task_center._core.primitives import TaskCenterLifecycleConfig
from task_center.agent_launch.composer import AgentEntryComposer
from task_center.context_engine.core import ContextEngine, ContextEngineDeps
from task_center._core.agent_routing import (
    PredicateRegistry,
    register_builtin_predicates,
)
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.context_engine.recipes_registry import RecipeRegistry
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.runtime import (
    AgentLaunch,
    AttemptDeps,
)
from task_center.iteration.state import IterationCreationReason


REPO_ROOT = next(
    parent
    for parent in Path(__file__).resolve().parents
    if (parent / "backend" / "src" / "agents").is_dir()
)
AGENTS_ROOT = REPO_ROOT / "backend" / "src" / "agents" / "profile"


class _RecordingLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:  # type: ignore[override]
        self.launches.append(launch)


@pytest.fixture(autouse=True)
def _isolate_global_registries():
    saved_predicates = dict(PredicateRegistry._registry)
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = list_definitions()
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    _clear_definitions()
    register_builtin_predicates()
    register_builtin_recipes()
    # Load every agent.md in the repo so resolver target lookups succeed.
    for definition in load_agents_tree(AGENTS_ROOT):
        register_definition(definition)
    yield
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    _clear_definitions()
    PredicateRegistry._registry.update(saved_predicates)
    RecipeRegistry._registry.update(saved_recipes)
    for definition in saved_definitions:
        register_definition(definition)


def _clear_definitions() -> None:
    for definition in list_definitions():
        unregister_definition(definition.name)


def _runtime_with_composer(
    goal_store, iteration_store, attempt_store, task_store
) -> tuple[AttemptDeps, _RecordingLauncher]:
    launcher = _RecordingLauncher()
    deps = ContextEngineDeps(
        goal_store=goal_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )
    composer = AgentEntryComposer.default(ContextEngine(deps))
    runtime = AttemptDeps(
        goal_store=goal_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=AttemptOrchestratorRegistry(),
        manager_registry=None,
        lifecycle_config=TaskCenterLifecycleConfig(),
        composer=composer,
    )
    return runtime, launcher


def _seed_partial_plan_caller(
    goal_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
):
    parent_req = goal_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal="parent",
    )
    parent_seg = iteration_store.insert(
        goal_id=parent_req.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="parent seg",
        attempt_budget=2,
    )
    caller_attempt = attempt_store.insert(
        iteration_id=parent_seg.id, attempt_sequence_no=1
    )
    attempt_store.set_plan_contract(
        caller_attempt.id,
        plan_spec="parent spec",
        evaluation_criteria=["c"],
        next_iteration_handoff_goal="continue here",
    )
    task_store.upsert_task(
        task_id="t-caller",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        context_message="x",
        status="running",
        summaries=[],
        needs=[],
        task_center_attempt_id=caller_attempt.id,
        spawn_reason="attempt_generator",
    )
    return parent_req


def test_partial_plan_caller_forks_child_planner_to_full_only(
    goal_store, iteration_store, attempt_store, task_store, task_center_run_id
):
    runtime, launcher = _runtime_with_composer(
        goal_store, iteration_store, attempt_store, task_store
    )
    _seed_partial_plan_caller(
        goal_store, iteration_store, attempt_store, task_store, task_center_run_id
    )

    # Child request spawned by the partial-plan caller task.
    child_req = goal_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-caller",
        goal="child",
    )
    child_seg = iteration_store.insert(
        goal_id=child_req.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="child seg",
        attempt_budget=2,
    )
    child_graph = attempt_store.insert(
        iteration_id=child_seg.id, attempt_sequence_no=1
    )
    orchestrator = AttemptOrchestrator(
        attempt=child_graph,
        on_attempt_closed=lambda _id: None,
        runtime=runtime,
    )
    orchestrator.start()

    assert len(launcher.launches) == 1
    launched = launcher.launches[0]

    # (a) selected agent is planner_full_only.
    assert launched.agent_name == "planner_full_only"
    # (b) the registered planner_full_only definition's terminals list does
    #     not include submit_plan_continues_goal (the gate is the agent.md filter).
    full_only = get_definition("planner_full_only")
    assert full_only is not None
    assert full_only.system_prompt is not None
    assert "Continuing the goal is disabled" in full_only.system_prompt
    assert "submit_plan_closes_goal" in full_only.terminals
    assert "submit_plan_continues_goal" not in full_only.terminals
