"""Phase 02 integration through handler -> manager -> orchestrator."""

from __future__ import annotations

from task_center.config import TaskCenterLifecycleConfig
from task_center.mission.handler import MissionHandler
from task_center.mission.mission import MissionStatus
from task_center.attempt import AttemptStatus
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.runtime import (
    AgentLaunch,
    AttemptDeps,
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
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.episode.episode import EpisodeStatus


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def _build_handler(
    mission_store,
    episode_store,
    attempt_store,
    task_store,
    *,
    composer,
):
    launcher = _FakeLauncher()
    orchestrator_registry = AttemptOrchestratorRegistry()
    manager_registry = EpisodeManagerRegistry()
    runtime = AttemptDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=orchestrator_registry,
        manager_registry=manager_registry,
        composer=composer,
    )
    handler = MissionHandler(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        manager_registry=manager_registry,
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
        orchestrator_factory=lambda attempt, on_attempt_closed: AttemptOrchestrator(
            attempt=attempt,
            on_attempt_closed=on_attempt_closed,
            runtime=runtime,
        ),
    )
    return handler, manager_registry, orchestrator_registry


def _plan(attempt_id: str) -> PlannerSubmission:
    return PlannerSubmission(
        attempt_id=attempt_id,
        planner_task_id=planner_task_id(attempt_id),
        kind="full",
        task_specification="spec",
        evaluation_criteria=("criterion",),
        tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        continuation_goal=None,
        summary="plan",
    )


def _generator_success(attempt_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, "a"),
        outcome="success",
        summary="done",
        payload={},
    )


def _generator_failure(attempt_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, "a"),
        outcome="failure",
        summary="failed",
        payload={},
    )


def _evaluator_success(attempt_id: str) -> EvaluatorSubmission:
    return EvaluatorSubmission(
        attempt_id=attempt_id,
        task_id=evaluator_task_id(attempt_id),
        outcome="success",
        summary="pass",
        payload={},
    )


def test_full_plan_execution_success_closes_request_success(
    mission_store,
    episode_store,
    attempt_store,
    task_store,
    task_center_run_id,
    composer,
):
    handler, manager_registry, orchestrator_registry = _build_handler(
        mission_store, episode_store, attempt_store, task_store, composer=composer
    )
    request = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    episode, _ = handler.create_initial_episode_with_manager(mission_id=request.id)
    manager = manager_registry.get(episode.id)
    assert manager is not None
    attempt = manager.create_initial_attempt()
    orchestrator = orchestrator_registry.get_or_raise(attempt.id)

    orchestrator.apply_plan_submission(_plan(attempt.id))
    orchestrator.apply_generator_submission(_generator_success(attempt.id))
    orchestrator.apply_evaluator_submission(_evaluator_success(attempt.id))

    final_request = mission_store.get(request.id)
    final_segment = episode_store.get(episode.id)
    final_graph = attempt_store.get(attempt.id)
    assert final_request is not None and final_segment is not None
    assert final_graph is not None
    assert final_request.status == MissionStatus.SUCCEEDED
    assert final_segment.status == EpisodeStatus.SUCCEEDED
    assert final_graph.status == AttemptStatus.PASSED
    assert manager_registry.get(episode.id) is None


def test_generator_failure_retry_then_evaluator_success(
    mission_store,
    episode_store,
    attempt_store,
    task_store,
    task_center_run_id,
    composer,
):
    handler, manager_registry, orchestrator_registry = _build_handler(
        mission_store, episode_store, attempt_store, task_store, composer=composer
    )
    request = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    episode, _ = handler.create_initial_episode_with_manager(mission_id=request.id)
    manager = manager_registry.get(episode.id)
    assert manager is not None
    graph1 = manager.create_initial_attempt()
    orchestrator1 = orchestrator_registry.get_or_raise(graph1.id)

    orchestrator1.apply_plan_submission(_plan(graph1.id))
    orchestrator1.apply_generator_submission(_generator_failure(graph1.id))

    refreshed_segment = episode_store.get(episode.id)
    assert refreshed_segment is not None
    assert len(refreshed_segment.attempt_ids) == 2
    graph2_id = refreshed_segment.attempt_ids[1]
    orchestrator2 = orchestrator_registry.get_or_raise(graph2_id)

    orchestrator2.apply_plan_submission(_plan(graph2_id))
    orchestrator2.apply_generator_submission(_generator_success(graph2_id))
    orchestrator2.apply_evaluator_submission(_evaluator_success(graph2_id))

    final_request = mission_store.get(request.id)
    final_segment = episode_store.get(episode.id)
    final_graph2 = attempt_store.get(graph2_id)
    assert final_request is not None and final_segment is not None
    assert final_graph2 is not None
    assert final_request.status == MissionStatus.SUCCEEDED
    assert final_segment.status == EpisodeStatus.SUCCEEDED
    assert final_graph2.status == AttemptStatus.PASSED
