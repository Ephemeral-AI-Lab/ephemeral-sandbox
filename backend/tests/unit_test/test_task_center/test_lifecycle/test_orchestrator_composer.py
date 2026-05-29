"""US-014: orchestrator + stage advancer composer wiring.

Confirms that when ``AttemptDeps.composer`` is set, the orchestrator
asks the composer for the planner agent name and context_message, and that
planner terminals are restricted when ancestry is nested.
"""

from __future__ import annotations


import pytest

from agents import (
    AgentDefinition,
    AgentKind,
    list_definitions,
    register_definition,
    unregister_definition,
)
from task_center._core.primitives import TaskCenterLifecycleConfig
from task_center.agent_launch.composer import AgentEntryComposer
from task_center.context_engine.core import ContextEngine, ContextEngineDeps
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.context_engine.recipes_registry import RecipeRegistry
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.deps import (
    AgentLaunch,
    AttemptDeps,
)
from task_center.iteration.state import IterationCreationReason


class _RecordingLauncher:
    """Captures launches without actually starting any agent run."""

    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:  # type: ignore[override]
        self.launches.append(launch)


@pytest.fixture(autouse=True)
def _isolate_global_registries():
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = list_definitions()
    RecipeRegistry.clear()
    _clear_definitions()
    register_builtin_recipes()
    yield
    RecipeRegistry.clear()
    _clear_definitions()
    RecipeRegistry._registry.update(saved_recipes)
    for definition in saved_definitions:
        register_definition(definition)


def _clear_definitions() -> None:
    for definition in list_definitions():
        unregister_definition(definition.name)


@pytest.fixture
def composer_runtime(
    workflow_store, iteration_store, attempt_store, task_store
) -> tuple[AttemptDeps, _RecordingLauncher]:
    launcher = _RecordingLauncher()
    deps = ContextEngineDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )
    composer = AgentEntryComposer.default(ContextEngine(deps))
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=AttemptOrchestratorRegistry(),
        iteration_coordinators=None,
        lifecycle_config=TaskCenterLifecycleConfig(),
        composer=composer,
    )
    return runtime, launcher


def _register_planner_agents() -> None:
    planner = AgentDefinition(
        name="planner",
        description="planner",
        agent_kind=AgentKind.PLANNER,
        context_recipe="planner",
        terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
        tool_call_limit=10,
        system_prompt="PLANNER",
    )
    register_definition(planner)


def _seed_request_segment_graph(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    request = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task",
        goal="overall",
    )
    iteration = iteration_store.insert(
        workflow_id=request.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="seg goal",
        attempt_budget=2,
    )
    attempt = attempt_store.insert(
        iteration_id=iteration.id, attempt_sequence_no=1
    )
    return request, iteration, attempt


def _setup_partial_plan_ancestor(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
):
    """Ancestor caller submitted a partial plan → child planner should fork."""
    parent_req = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task",
        goal="parent",
    )
    parent_seg = iteration_store.insert(
        workflow_id=parent_req.id,
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
        plan_spec="caller spec",
        evaluation_criteria=["c"],
        deferred_goal_for_next_iteration="continue here",   # ← partial plan
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


def test_planner_launched_via_composer_uses_base_when_no_ancestor(
    composer_runtime,
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
):
    runtime, launcher = composer_runtime
    _register_planner_agents()
    request, iteration, attempt = _seed_request_segment_graph(
        workflow_store, iteration_store, attempt_store, task_center_run_id
    )
    orchestrator = AttemptOrchestrator(
        attempt=attempt, on_attempt_closed=lambda _id: None, runtime=runtime
    )
    orchestrator.start()
    assert len(launcher.launches) == 1
    launched = launcher.launches[0]
    assert launched.agent_name == "planner"
    assert launched.agent_def is not None
    assert launched.agent_def.system_prompt == "PLANNER"
    assert launched.agent_def.terminals == [
        "submit_plan_closes_goal",
        "submit_plan_defers_goal",
    ]
    assert launched.context_packet_id is None  # no packet store wired
    assert '<iteration iteration_no="1" position="current">' in launched.context
    assert "<iteration_goal>" in launched.context


def test_planner_terminals_restricted_when_partial_plan_caller_present(
    composer_runtime,
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
):
    runtime, launcher = composer_runtime
    _register_planner_agents()
    _setup_partial_plan_ancestor(
        workflow_store,
        iteration_store,
        attempt_store,
        task_store,
        task_center_run_id,
    )
    # Child request is spawned by the partial-plan caller task.
    child_req = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-caller",
        goal="child",
    )
    child_seg = iteration_store.insert(
        workflow_id=child_req.id,
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
    assert launched.agent_name == "planner"
    assert launched.agent_def is not None
    assert launched.agent_def.system_prompt == "PLANNER"
    assert launched.agent_def.terminals == ["submit_plan_closes_goal"]
