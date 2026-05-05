"""US-014: orchestrator + dispatcher composer wiring.

Confirms that when ``HarnessGraphRuntime.composer`` is set, the orchestrator
asks the composer for the planner agent name and task_input, and that
``planner_full_only`` is selected when ancestry has a partial-plan caller.
"""

from __future__ import annotations


import pytest

from agents import registry as agents_registry
from agents.types import (
    AgentDefinition,
    AgentVariant,
)
from task_center.config import HarnessLifecycleConfig
from task_center.context_engine.composer import ContextComposer
from task_center.context_engine.engine import ContextEngine, ContextEngineDeps
from task_center.agent_launch.predicates import (
    PredicateRegistry,
    register_builtin_predicates,
)
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.context_engine.recipes_registry import RecipeRegistry
from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import (
    AgentLaunch,
    HarnessGraphRuntime,
)
from task_center.episode.episode import TaskSegmentCreationReason


class _RecordingLauncher:
    """Captures launches without actually starting any agent run."""

    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:  # type: ignore[override]
        self.launches.append(launch)


@pytest.fixture(autouse=True)
def _isolate_global_registries():
    saved_predicates = dict(PredicateRegistry._registry)
    saved_recipes = dict(RecipeRegistry._registry)
    saved_definitions = dict(agents_registry._DEFINITIONS)
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    agents_registry._DEFINITIONS.clear()
    register_builtin_predicates()
    register_builtin_recipes()
    yield
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    agents_registry._DEFINITIONS.clear()
    PredicateRegistry._registry.update(saved_predicates)
    RecipeRegistry._registry.update(saved_recipes)
    agents_registry._DEFINITIONS.update(saved_definitions)


@pytest.fixture
def composer_runtime(
    request_store, segment_store, graph_store, task_store
) -> tuple[HarnessGraphRuntime, _RecordingLauncher]:
    launcher = _RecordingLauncher()
    deps = ContextEngineDeps(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
    )
    composer = ContextComposer.default(ContextEngine(deps))
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=HarnessGraphOrchestratorRegistry(),
        manager_registry=None,
        lifecycle_config=HarnessLifecycleConfig(),
        composer=composer,
    )
    return runtime, launcher


def _register_planner_agents() -> None:
    base = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        terminals=["submit_full_plan", "submit_partial_plan"],
        variants=[
            AgentVariant(
                when="partial_plan_caller_ancestor",
                use="planner_full_only",
            )
        ],
        system_prompt="PLANNER",
    )
    full_only = AgentDefinition(
        name="planner_full_only",
        description="planner",
        context_recipe="planner_v1",
        terminals=["submit_full_plan"],
        system_prompt="PLANNER FULL ONLY",
    )
    agents_registry.register_definition(base)
    agents_registry.register_definition(full_only)


def _seed_request_segment_graph(
    request_store, segment_store, graph_store, task_center_run_id
):
    request = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal="overall",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="seg goal",
        attempt_budget=2,
    )
    graph = graph_store.insert(
        task_segment_id=segment.id, graph_sequence_no=1
    )
    return request, segment, graph


def _setup_partial_plan_ancestor(
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
):
    """Ancestor caller submitted a partial plan → child planner should fork."""
    parent_req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal="parent",
    )
    parent_seg = segment_store.insert(
        complex_task_request_id=parent_req.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="parent seg",
        attempt_budget=2,
    )
    caller_graph = graph_store.insert(
        task_segment_id=parent_seg.id, graph_sequence_no=1
    )
    graph_store.set_plan_contract(
        caller_graph.id,
        task_specification="caller spec",
        evaluation_criteria=["c"],
        continuation_goal="continue here",   # ← partial plan
    )
    task_store.upsert_task(
        task_id="t-caller",
        task_center_run_id=task_center_run_id,
        role="generator",
        agent_name="executor",
        task_input="x",
        status="running",
        summaries=[],
        needs=[],
        task_center_harness_graph_id=caller_graph.id,
        spawn_reason="harness_graph_generator",
    )
    return parent_req


def test_planner_launched_via_composer_uses_base_when_no_ancestor(
    composer_runtime,
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
):
    runtime, launcher = composer_runtime
    _register_planner_agents()
    request, segment, graph = _seed_request_segment_graph(
        request_store, segment_store, graph_store, task_center_run_id
    )
    orchestrator = HarnessGraphOrchestrator(
        harness_graph=graph, on_graph_closed=lambda _id: None, runtime=runtime
    )
    orchestrator.start()
    assert len(launcher.launches) == 1
    launched = launcher.launches[0]
    assert launched.agent_name == "planner"
    assert launched.system_prompt == "PLANNER"
    assert launched.context_packet_id is None  # no packet store wired
    assert "Mission / Current Episode" in launched.task_input


def test_planner_forked_to_full_only_when_partial_plan_caller_present(
    composer_runtime,
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
):
    runtime, launcher = composer_runtime
    _register_planner_agents()
    _setup_partial_plan_ancestor(
        request_store,
        segment_store,
        graph_store,
        task_store,
        task_center_run_id,
    )
    # Child request is spawned by the partial-plan caller task.
    child_req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-caller",
        goal="child",
    )
    child_seg = segment_store.insert(
        complex_task_request_id=child_req.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="child seg",
        attempt_budget=2,
    )
    child_graph = graph_store.insert(
        task_segment_id=child_seg.id, graph_sequence_no=1
    )
    orchestrator = HarnessGraphOrchestrator(
        harness_graph=child_graph,
        on_graph_closed=lambda _id: None,
        runtime=runtime,
    )
    orchestrator.start()
    assert len(launcher.launches) == 1
    launched = launcher.launches[0]
    assert launched.agent_name == "planner_full_only"
    assert launched.system_prompt == "PLANNER FULL ONLY"
