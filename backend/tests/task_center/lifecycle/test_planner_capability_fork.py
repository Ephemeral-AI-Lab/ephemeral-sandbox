"""US-018: end-to-end planner capability fork.

Builds a parent request whose harness graph submitted a partial plan, spawns
a child request, then asserts the planner spawned for the child:

* is the ``planner_full_only`` agent (resolver swapped via the variant);
* receives a full-only system prompt from the selected agent definition;
* the registered ``planner_full_only`` AgentDefinition has
  ``terminals`` without ``submit_partial_plan`` (the gate is the agent.md
  ``terminals:`` filter — the model never sees the tool when the variant
  fires).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from agents import registry as agents_registry
from agents.loader import load_agents_tree
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


REPO_ROOT = Path(__file__).resolve().parents[4]
AGENTS_ROOT = REPO_ROOT / "backend" / "src" / "agents"


class _RecordingLauncher:
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
    # Load every agent.md in the repo so resolver target lookups succeed.
    for definition in load_agents_tree(AGENTS_ROOT):
        agents_registry.register_definition(definition)
    yield
    PredicateRegistry.clear()
    RecipeRegistry.clear()
    agents_registry._DEFINITIONS.clear()
    PredicateRegistry._registry.update(saved_predicates)
    RecipeRegistry._registry.update(saved_recipes)
    agents_registry._DEFINITIONS.update(saved_definitions)


def _runtime_with_composer(
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


def _seed_partial_plan_caller(
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
):
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
        task_specification="parent spec",
        evaluation_criteria=["c"],
        continuation_goal="continue here",
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


def test_partial_plan_caller_forks_child_planner_to_full_only(
    request_store, segment_store, graph_store, task_store, task_center_run_id
):
    runtime, launcher = _runtime_with_composer(
        request_store, segment_store, graph_store, task_store
    )
    _seed_partial_plan_caller(
        request_store, segment_store, graph_store, task_store, task_center_run_id
    )

    # Child request spawned by the partial-plan caller task.
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

    # (a) selected agent is planner_full_only.
    assert launched.agent_name == "planner_full_only"
    # (b) full-only policy text comes from the selected agent definition.
    assert "Partial planning is disabled" in launched.system_prompt
    # (c) the registered planner_full_only definition's terminals list does
    #     not include submit_partial_plan (the gate is the agent.md filter).
    full_only = agents_registry.get_definition("planner_full_only")
    assert full_only is not None
    assert "submit_full_plan" in full_only.terminals
    assert "submit_partial_plan" not in full_only.terminals
