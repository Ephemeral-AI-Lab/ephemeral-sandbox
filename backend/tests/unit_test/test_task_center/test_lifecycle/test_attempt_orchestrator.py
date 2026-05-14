"""AttemptOrchestrator lifecycle tests."""

from __future__ import annotations

import pytest

from task_center.mission.mission import MissionCloseReport
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.attempt import (
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
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
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
    PlannedGeneratorTask,
    PlannerFailureSubmission,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)
from task_center.episode.episode import EpisodeCreationReason


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


class _FailingRoleLauncher(_FakeLauncher):
    def __init__(self, role: TaskCenterTaskRole) -> None:
        super().__init__()
        self._role = role

    def launch(self, launch: AgentLaunch) -> None:
        if launch.role == self._role:
            raise RuntimeError(f"{self._role.value} launch failed")
        super().launch(launch)


class _FailingEvaluatorComposer:
    def __init__(self, inner) -> None:
        self._inner = inner
        self.engine = inner.engine

    def compose(self, *, base_agent_name: str, scope):
        if base_agent_name == "evaluator":
            raise RuntimeError("evaluator compose failed")
        return self._inner.compose(base_agent_name=base_agent_name, scope=scope)


def _seed_graph(mission_store, episode_store, attempt_store, task_center_run_id):
    request = mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="outer-task",
        goal="solve the task",
    )
    episode = episode_store.insert(
        mission_id=request.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="solve the task",
        attempt_budget=2,
    )
    return attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)


def _build_orchestrator(
    mission_store,
    episode_store,
    attempt_store,
    task_store,
    task_center_run_id,
    *,
    composer,
    launcher=None,
):
    attempt = _seed_graph(
        mission_store, episode_store, attempt_store, task_center_run_id
    )
    launcher = launcher or _FakeLauncher()
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        composer=composer,
    )
    closed: list[str] = []
    orchestrator = AttemptOrchestrator(
        attempt=attempt,
        on_attempt_closed=closed.append,
        runtime=runtime,
    )
    registry.register(orchestrator)
    return orchestrator, attempt, launcher, registry, closed


def _plan(
    attempt_id: str,
    *,
    tasks: tuple[PlannedGeneratorTask, ...],
    kind: str = "full",
    continuation_goal: str | None = None,
) -> PlannerSubmission:
    return PlannerSubmission(
        attempt_id=attempt_id,
        planner_task_id=planner_task_id(attempt_id),
        kind=kind,  # type: ignore[arg-type]
        task_specification="spec",
        evaluation_criteria=("criterion",),
        tasks=tasks,
        continuation_goal=continuation_goal,
        summary="plan accepted",
    )


def _generator_success(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        outcome="success",
        summary=f"{local_id} done",
        payload={"role": "executor"},
    )


def _generator_failure(attempt_id: str, local_id: str) -> GeneratorSubmission:
    return GeneratorSubmission(
        attempt_id=attempt_id,
        task_id=generator_task_id(attempt_id, local_id),
        outcome="failure",
        summary=f"{local_id} failed",
        payload={"role": "executor"},
    )


def _evaluator_submission(attempt_id: str, outcome: str) -> EvaluatorSubmission:
    return EvaluatorSubmission(
        attempt_id=attempt_id,
        task_id=evaluator_task_id(attempt_id),
        outcome=outcome,  # type: ignore[arg-type]
        summary=f"evaluation {outcome}",
        payload={},
    )


def test_start_creates_planner_task_and_sets_graph_planner_id(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )

    orchestrator.start()

    refreshed = attempt_store.get(attempt.id)
    task_id = planner_task_id(attempt.id)
    planner_task = task_store.get_task(task_id)
    assert refreshed is not None and refreshed.planner_task_id == task_id
    assert planner_task is not None
    assert planner_task["status"] == TaskCenterTaskStatus.RUNNING.value
    assert [launch.task_id for launch in launcher.launches] == [task_id]


def test_apply_plan_submission_persists_contract_and_generator_ids(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    tasks = (
        PlannedGeneratorTask("a", "executor", (), "do A"),
        PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
    )

    orchestrator.apply_plan_submission(_plan(attempt.id, tasks=tasks))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.GENERATE
    assert refreshed.task_specification == "spec"
    assert refreshed.generator_task_ids == (
        generator_task_id(attempt.id, "a"),
        generator_task_id(attempt.id, "b"),
    )
    assert [launch.task_id for launch in launcher.launches] == [
        planner_task_id(attempt.id),
        generator_task_id(attempt.id, "a"),
    ]


def test_apply_partial_plan_submission_stores_continuation_goal(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()

    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
            kind="partial",
            continuation_goal="continue here",
        )
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.continuation_goal == "continue here"


def test_apply_planner_failure_marks_task_and_closes_graph(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()

    orchestrator.apply_planner_failure(
        PlannerFailureSubmission(
            attempt_id=attempt.id,
            planner_task_id=planner_task_id(attempt.id),
            fail_reason="run_exhausted",
            summary="planner stopped",
        )
    )

    refreshed = attempt_store.get(attempt.id)
    task = task_store.get_task(planner_task_id(attempt.id))
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.PLANNER_FAILED
    assert task is not None and task["status"] == TaskCenterTaskStatus.FAILED.value
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_apply_generator_success_launches_newly_ready_dependents(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    assert [launch.task_id for launch in launcher.launches] == [
        planner_task_id(attempt.id),
        generator_task_id(attempt.id, "a"),
        generator_task_id(attempt.id, "b"),
    ]
    task_b = task_store.get_task(generator_task_id(attempt.id, "b"))
    assert task_b is not None and task_b["status"] == "running"


def test_missing_generator_agent_profile_is_invariant_violation(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "verifier", ("a",), "verify A"),
            ),
        )
    )
    task_b_id = generator_task_id(attempt.id, "b")
    task_b = task_store.get_task(task_b_id)
    assert task_b is not None
    task_store.upsert_task(
        task_id=task_b_id,
        task_center_run_id=task_b["task_center_run_id"],
        role=task_b["role"],
        agent_name=None,
        rendered_prompt=task_b["rendered_prompt"],
        status=task_b["status"],
        summaries=task_b["summaries"],
        needs=task_b["needs"],
        task_center_attempt_id=task_b["task_center_attempt_id"],
        spawn_reason=task_b["spawn_reason"],
    )

    with pytest.raises(TaskCenterInvariantViolation):
        orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    refreshed_task_b = task_store.get_task(task_b_id)
    assert refreshed_task_b is not None
    assert refreshed_task_b["status"] == TaskCenterTaskStatus.PENDING.value


def test_generator_launch_failure_marks_task_failed_and_closes_graph(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    launcher = _FailingRoleLauncher(TaskCenterTaskRole.GENERATOR)
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        mission_store,
        episode_store,
        attempt_store,
        task_store,
        task_center_run_id,
        launcher=launcher,
        composer=composer,
    )
    orchestrator.start()

    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    task = task_store.get_task(generator_task_id(attempt.id, "a"))
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["summaries"][-1]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.GENERATOR_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_evaluator_launch_failure_marks_task_failed_and_closes_graph(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    launcher = _FailingRoleLauncher(TaskCenterTaskRole.EVALUATOR)
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        mission_store,
        episode_store,
        attempt_store,
        task_store,
        task_center_run_id,
        launcher=launcher,
        composer=composer,
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    task = task_store.get_task(evaluator_task_id(attempt.id))
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["summaries"][-1]["fail_reason"] == "agent_launch_failed"
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.EVALUATOR_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_evaluator_compose_failure_closes_graph(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, registry, closed = _build_orchestrator(
        mission_store,
        episode_store,
        attempt_store,
        task_store,
        task_center_run_id,
        composer=_FailingEvaluatorComposer(composer),
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    with pytest.raises(RuntimeError, match="evaluator compose failed"):
        orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    task = task_store.get_task(evaluator_task_id(attempt.id))
    refreshed = attempt_store.get(attempt.id)
    assert task is None
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.EVALUATOR_FAILED
    assert closed == [attempt.id]
    assert registry.get(attempt.id) is None


def test_waiting_mission_prevents_generator_quiescence(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", (), "do B"),
            ),
        )
    )
    task_store.set_task_status(
        generator_task_id(attempt.id, "a"),
        status=TaskCenterTaskStatus.WAITING_MISSION.value,
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "b"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.GENERATE
    assert closed == []


def test_mission_close_report_success_resumes_waiting_generator(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    task_id = generator_task_id(attempt.id, "a")
    task_store.set_task_status(
        task_id,
        status=TaskCenterTaskStatus.WAITING_MISSION.value,
    )

    orchestrator.apply_mission_close_report(
        MissionCloseReport(
            mission_id="delegated-1",
            requested_by_task_id=task_id,
            outcome="success",
            final_episode_id="episode-1",
            final_attempt_id="attempt-1",
        )
    )

    task = task_store.get_task(task_id)
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert task["summaries"][-1]["payload"]["mission_close_report"][
        "mission_id"
    ] == "delegated-1"
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.EVALUATE


def test_mission_close_report_failure_blocks_dependents_and_closes_graph(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
            ),
        )
    )
    task_id = generator_task_id(attempt.id, "a")
    dependent_id = generator_task_id(attempt.id, "b")
    task_store.set_task_status(
        task_id,
        status=TaskCenterTaskStatus.WAITING_MISSION.value,
    )

    orchestrator.apply_mission_close_report(
        MissionCloseReport(
            mission_id="delegated-1",
            requested_by_task_id=task_id,
            outcome="failed",
            final_episode_id="episode-1",
            final_attempt_id="attempt-1",
        )
    )

    task = task_store.get_task(task_id)
    dependent = task_store.get_task(dependent_id)
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert dependent is not None
    assert dependent["status"] == TaskCenterTaskStatus.BLOCKED.value
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert closed == [attempt.id]


def test_apply_generator_failure_blocks_pending_descendants(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
                PlannedGeneratorTask("c", "executor", ("b",), "do C"),
                PlannedGeneratorTask("d", "executor", (), "do D"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_failure(attempt.id, "a"))

    task_b = task_store.get_task(generator_task_id(attempt.id, "b"))
    task_c = task_store.get_task(generator_task_id(attempt.id, "c"))
    task_d = task_store.get_task(generator_task_id(attempt.id, "d"))
    assert task_b is not None and task_b["status"] == "blocked"
    assert task_c is not None and task_c["status"] == "blocked"
    assert task_d is not None and task_d["status"] == "running"


def test_generator_failure_waits_then_closes_after_quiescence(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", (), "do B"),
            ),
        )
    )

    orchestrator.apply_generator_submission(_generator_failure(attempt.id, "a"))
    assert closed == []

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "b"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.GENERATOR_FAILED
    assert closed == [attempt.id]


def test_all_generators_done_spawns_evaluator(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, launcher, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )

    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.stage == AttemptStage.EVALUATE
    assert launcher.launches[-1].task_id == evaluator_task_id(attempt.id)


def test_apply_evaluator_success_closes_graph_passed(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    orchestrator.apply_evaluator_submission(
        _evaluator_submission(attempt.id, "success")
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.PASSED
    assert refreshed.fail_reason is None
    assert closed == [attempt.id]


def test_apply_evaluator_failure_closes_graph_failed(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, closed = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_success(attempt.id, "a"))

    orchestrator.apply_evaluator_submission(
        _evaluator_submission(attempt.id, "failure")
    )

    refreshed = attempt_store.get(attempt.id)
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.EVALUATOR_FAILED
    assert closed == [attempt.id]


def test_orchestrator_never_creates_retry_attempt(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
):
    orchestrator, attempt, _, _, _ = _build_orchestrator(
        mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer=composer
    )
    orchestrator.start()
    orchestrator.apply_plan_submission(
        _plan(
            attempt.id,
            tasks=(PlannedGeneratorTask("a", "executor", (), "do A"),),
        )
    )
    orchestrator.apply_generator_submission(_generator_failure(attempt.id, "a"))

    assert len(attempt_store.list_for_episode(attempt.episode_id)) == 1
