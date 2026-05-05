"""Phase 02 integration through handler -> manager -> orchestrator."""

from __future__ import annotations

from task_center.config import HarnessLifecycleConfig
from task_center.mission.handler import ComplexTaskRequestHandler
from task_center.mission.mission import ComplexTaskRequestStatus
from task_center.attempt.factory import (
    make_attempt_orchestrator_factory,
)
from task_center.attempt import HarnessGraphStatus
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import (
    AgentLaunch,
    HarnessGraphRuntime,
)
from task_center.task import (
    EvaluatorSubmission,
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)
from task_center.episode.registry import SegmentManagerRegistry
from task_center.episode.episode import TaskSegmentStatus


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def _build_handler(
    request_store,
    segment_store,
    graph_store,
    task_store,
    *,
    composer,
):
    launcher = _FakeLauncher()
    orchestrator_registry = HarnessGraphOrchestratorRegistry()
    manager_registry = SegmentManagerRegistry()
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=orchestrator_registry,
        manager_registry=manager_registry,
        composer=composer,
    )
    handler = ComplexTaskRequestHandler(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        manager_registry=manager_registry,
        config=HarnessLifecycleConfig(default_attempt_budget=2),
        orchestrator_factory=make_attempt_orchestrator_factory(
            runtime=runtime,
        ),
    )
    return handler, manager_registry, orchestrator_registry


def _plan(graph_id: str) -> PlannerSubmission:
    return PlannerSubmission(
        graph_id=graph_id,
        planner_task_id=planner_task_id(graph_id),
        kind="full",
        task_specification="spec",
        evaluation_criteria=("criterion",),
        tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        continuation_goal=None,
        summary="plan",
    )


def _generator_success(graph_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        graph_id=graph_id,
        task_id=generator_task_id(graph_id, "a"),
        outcome="success",
        summary="done",
        payload={},
    )


def _generator_failure(graph_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        graph_id=graph_id,
        task_id=generator_task_id(graph_id, "a"),
        outcome="failure",
        summary="failed",
        payload={},
    )


def _evaluator_success(graph_id: str) -> EvaluatorSubmission:
    return EvaluatorSubmission(
        graph_id=graph_id,
        task_id=evaluator_task_id(graph_id),
        outcome="success",
        summary="pass",
        payload={},
    )


def test_full_plan_execution_success_closes_request_success(
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
    composer,
):
    handler, manager_registry, orchestrator_registry = _build_handler(
        request_store, segment_store, graph_store, task_store, composer=composer
    )
    request = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    segment = handler.create_initial_episode(complex_task_request_id=request.id)
    manager = manager_registry.get(segment.id)
    assert manager is not None
    graph = manager.create_initial_attempt()
    orchestrator = orchestrator_registry.get_or_raise(graph.id)

    orchestrator.apply_plan_submission(_plan(graph.id))
    orchestrator.apply_generator_submission(_generator_success(graph.id))
    orchestrator.apply_evaluator_submission(_evaluator_success(graph.id))

    final_request = request_store.get(request.id)
    final_segment = segment_store.get(segment.id)
    final_graph = graph_store.get(graph.id)
    assert final_request is not None and final_segment is not None
    assert final_graph is not None
    assert final_request.status == ComplexTaskRequestStatus.SUCCEEDED
    assert final_segment.status == TaskSegmentStatus.SUCCEEDED
    assert final_graph.status == HarnessGraphStatus.PASSED
    assert manager_registry.get(segment.id) is None


def test_generator_failure_retry_then_evaluator_success(
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
    composer,
):
    handler, manager_registry, orchestrator_registry = _build_handler(
        request_store, segment_store, graph_store, task_store, composer=composer
    )
    request = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    segment = handler.create_initial_episode(complex_task_request_id=request.id)
    manager = manager_registry.get(segment.id)
    assert manager is not None
    graph1 = manager.create_initial_attempt()
    orchestrator1 = orchestrator_registry.get_or_raise(graph1.id)

    orchestrator1.apply_plan_submission(_plan(graph1.id))
    orchestrator1.apply_generator_submission(_generator_failure(graph1.id))

    refreshed_segment = segment_store.get(segment.id)
    assert refreshed_segment is not None
    assert len(refreshed_segment.harness_graph_ids) == 2
    graph2_id = refreshed_segment.harness_graph_ids[1]
    orchestrator2 = orchestrator_registry.get_or_raise(graph2_id)

    orchestrator2.apply_plan_submission(_plan(graph2_id))
    orchestrator2.apply_generator_submission(_generator_success(graph2_id))
    orchestrator2.apply_evaluator_submission(_evaluator_success(graph2_id))

    final_request = request_store.get(request.id)
    final_segment = segment_store.get(segment.id)
    final_graph2 = graph_store.get(graph2_id)
    assert final_request is not None and final_segment is not None
    assert final_graph2 is not None
    assert final_request.status == ComplexTaskRequestStatus.SUCCEEDED
    assert final_segment.status == TaskSegmentStatus.SUCCEEDED
    assert final_graph2.status == HarnessGraphStatus.PASSED
