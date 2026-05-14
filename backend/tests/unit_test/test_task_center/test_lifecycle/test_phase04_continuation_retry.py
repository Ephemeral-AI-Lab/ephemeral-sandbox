"""Phase 04 end-to-end continuation and retry tests.

Drives the full coordinator → handler → manager → orchestrator pipeline so
that retry, continuation, and final close-report routing are exercised
together. The parent task must remain in ``waiting_mission`` until the
delegated mission closes terminally.
"""

from __future__ import annotations

from task_center.mission.starter import MissionStarter
from task_center.mission.mission import MissionStatus
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt import (
    AttemptFailReason,
    AttemptStatus,
)
from task_center.attempt.runtime import AgentLaunch, AttemptDeps
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.episode.episode import (
    EpisodeCreationReason,
    EpisodeStatus,
)
from task_center.task import (
    EvaluatorSubmission,
    GeneratorSubmission,
    TaskCenterTaskStatus,
    PlannedGeneratorTask,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


class _FailOnLaunchNumber(_FakeLauncher):
    def __init__(self, fail_on: int) -> None:
        super().__init__()
        self._fail_on = fail_on

    def launch(self, launch: AgentLaunch) -> None:
        super().launch(launch)
        if len(self.launches) == self._fail_on:
            raise RuntimeError("planned launch failure")


def _build_runtime(
    mission_store, episode_store, attempt_store, task_store, *, composer, launcher=None
) -> AttemptDeps:
    return AttemptDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher or _FakeLauncher(),
        orchestrator_registry=AttemptOrchestratorRegistry(),
        manager_registry=EpisodeManagerRegistry(),
        composer=composer,
    )


def _seed_outer_running_generator(
    *,
    runtime: AttemptDeps,
    mission_store,
    episode_store,
    attempt_store,
    task_store,
    task_center_run_id: str,
) -> tuple[str, str]:
    """Seed an outer parent attempt + a single running generator task on it."""
    outer_request = mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="root",
        goal="outer goal",
    )
    outer_segment = episode_store.insert(
        mission_id=outer_request.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="outer goal",
        attempt_budget=2,
    )
    mission_store.append_episode_id(outer_request.id, outer_segment.id)
    outer_attempt = attempt_store.insert(
        episode_id=outer_segment.id, attempt_sequence_no=1
    )
    episode_store.append_attempt_id(outer_segment.id, outer_attempt.id)
    outer_orchestrator = AttemptOrchestrator(
        attempt=outer_attempt,
        on_attempt_closed=lambda attempt_id: None,
        runtime=runtime,
    )
    runtime.orchestrator_registry.register(outer_orchestrator)
    outer_orchestrator.start()
    outer_orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=outer_attempt.id,
            planner_task_id=planner_task_id(outer_attempt.id),
            kind="full",
            task_specification="outer spec",
            evaluation_criteria=("outer ok",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="outer",
                    agent_name="executor",
                    deps=(),
                    task_spec="execute outer",
                ),
            ),
            continuation_goal=None,
            summary="outer plan",
        )
    )
    parent_task_id = generator_task_id(outer_attempt.id, "outer")
    return parent_task_id, outer_attempt.id


def _drive_delegated_attempt_to_pass(
    *,
    runtime: AttemptDeps,
    delegated_attempt_id: str,
    continuation_goal: str | None,
) -> None:
    """Plan, execute, and pass the delegated attempt."""
    delegated = runtime.orchestrator_registry.get_or_raise(delegated_attempt_id)
    if continuation_goal is None:
        delegated.apply_plan_submission(
            PlannerSubmission(
                attempt_id=delegated_attempt_id,
                planner_task_id=planner_task_id(delegated_attempt_id),
                kind="full",
                task_specification="delegated spec",
                evaluation_criteria=("delegated ok",),
                tasks=(
                    PlannedGeneratorTask(
                        local_id="d",
                        agent_name="executor",
                        deps=(),
                        task_spec="do delegated",
                    ),
                ),
                continuation_goal=None,
                summary="delegated plan",
            )
        )
    else:
        delegated.apply_plan_submission(
            PlannerSubmission(
                attempt_id=delegated_attempt_id,
                planner_task_id=planner_task_id(delegated_attempt_id),
                kind="partial",
                task_specification="delegated spec",
                evaluation_criteria=("delegated ok",),
                tasks=(
                    PlannedGeneratorTask(
                        local_id="d",
                        agent_name="executor",
                        deps=(),
                        task_spec="do delegated",
                    ),
                ),
                continuation_goal=continuation_goal,
                summary="delegated plan",
            )
        )
    delegated.apply_generator_submission(
        GeneratorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=generator_task_id(delegated_attempt_id, "d"),
            outcome="success",
            summary="generator ok",
            payload={},
        )
    )
    delegated.apply_evaluator_submission(
        EvaluatorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=evaluator_task_id(delegated_attempt_id),
            outcome="success",
            summary="evaluator ok",
            payload={},
        )
    )


def _drive_delegated_attempt_to_fail(
    *,
    runtime: AttemptDeps,
    delegated_attempt_id: str,
) -> None:
    delegated = runtime.orchestrator_registry.get_or_raise(delegated_attempt_id)
    delegated.apply_plan_submission(
        PlannerSubmission(
            attempt_id=delegated_attempt_id,
            planner_task_id=planner_task_id(delegated_attempt_id),
            kind="full",
            task_specification="delegated spec",
            evaluation_criteria=("delegated ok",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="d",
                    agent_name="executor",
                    deps=(),
                    task_spec="do delegated",
                ),
            ),
            continuation_goal=None,
            summary="delegated plan",
        )
    )
    delegated.apply_generator_submission(
        GeneratorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=generator_task_id(delegated_attempt_id, "d"),
            outcome="failure",
            summary="generator failed",
            payload={},
        )
    )


def test_delegated_continuation_waits_until_final_segment(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        mission_store, episode_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_running_generator(
        runtime=runtime,
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = MissionStarter(runtime=runtime)
    mission_start = coordinator.start(
        parent_task_id=parent_task_id,
        goal="delegated continuation",
    )

    segment1_initial_attempt_id = mission_start.initial_attempt_id

    # Segment 1 passes with continuation goal — parent must remain WAITING.
    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=segment1_initial_attempt_id,
        continuation_goal="continue work",
    )
    parent_after_segment1 = task_store.get_task(parent_task_id)
    assert parent_after_segment1 is not None
    assert (
        parent_after_segment1["status"]
        == TaskCenterTaskStatus.WAITING_MISSION.value
    )
    delegated_request_after_segment1 = mission_store.get(
        mission_start.mission_id
    )
    assert delegated_request_after_segment1 is not None
    assert delegated_request_after_segment1.status == MissionStatus.OPEN
    assert len(delegated_request_after_segment1.episode_ids) == 2

    # Segment 2 starts from the new continuation attempt the handler created.
    segment2_id = delegated_request_after_segment1.episode_ids[1]
    segment2 = episode_store.get(segment2_id)
    assert segment2 is not None
    assert segment2.goal == "continue work"
    segment2_initial_attempt_id = segment2.attempt_ids[0]
    # Drive episode 2 to terminal success.
    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=segment2_initial_attempt_id,
        continuation_goal=None,
    )

    parent_final = task_store.get_task(parent_task_id)
    delegated_final = mission_store.get(mission_start.mission_id)
    segment2_final = episode_store.get(segment2_id)
    assert parent_final is not None
    assert parent_final["status"] == TaskCenterTaskStatus.DONE.value
    assert delegated_final is not None
    assert delegated_final.status == MissionStatus.SUCCEEDED
    assert segment2_final is not None
    assert segment2_final.status == EpisodeStatus.SUCCEEDED


def test_continuation_startup_failure_reports_continuation_graph(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    launcher = _FailOnLaunchNumber(fail_on=6)
    runtime = _build_runtime(
        mission_store,
        episode_store,
        attempt_store,
        task_store,
        composer=composer,
        launcher=launcher,
    )
    parent_task_id, parent_attempt_id = _seed_outer_running_generator(
        runtime=runtime,
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = MissionStarter(runtime=runtime)
    mission_start = coordinator.start(
        parent_task_id=parent_task_id,
        goal="delegated continuation",
    )

    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=mission_start.initial_attempt_id,
        continuation_goal="continue work",
    )

    request = mission_store.get(mission_start.mission_id)
    assert request is not None
    assert request.status == MissionStatus.FAILED
    assert request.final_outcome is not None
    segment2_id = request.episode_ids[1]
    segment2 = episode_store.get(segment2_id)
    assert segment2 is not None
    failed_attempt_id = segment2.attempt_ids[0]
    failed_attempt = attempt_store.get(failed_attempt_id)
    assert failed_attempt is not None
    assert failed_attempt.status == AttemptStatus.FAILED
    assert failed_attempt.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert request.final_outcome["final_episode_id"] == segment2_id
    assert request.final_outcome["final_attempt_id"] == failed_attempt_id

    parent_final = task_store.get_task(parent_task_id)
    assert parent_final is not None
    assert parent_final["status"] == TaskCenterTaskStatus.FAILED.value


def test_delegated_retry_waits_until_final_graph(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        mission_store, episode_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_running_generator(
        runtime=runtime,
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = MissionStarter(runtime=runtime)
    mission_start = coordinator.start(
        parent_task_id=parent_task_id,
        goal="delegated retry",
    )

    # Graph 1 fails — manager should retry inside same episode, parent waits.
    _drive_delegated_attempt_to_fail(
        runtime=runtime, delegated_attempt_id=mission_start.initial_attempt_id
    )
    segment1 = episode_store.get(mission_start.initial_episode_id)
    assert segment1 is not None
    assert len(segment1.attempt_ids) == 2
    parent_mid = task_store.get_task(parent_task_id)
    assert parent_mid is not None
    assert parent_mid["status"] == TaskCenterTaskStatus.WAITING_MISSION.value
    delegated_mid = mission_store.get(mission_start.mission_id)
    assert delegated_mid is not None
    assert delegated_mid.status == MissionStatus.OPEN

    # Graph 2 passes terminally inside the same episode — final close.
    retry_attempt_id = segment1.attempt_ids[1]
    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=retry_attempt_id,
        continuation_goal=None,
    )

    parent_final = task_store.get_task(parent_task_id)
    delegated_final = mission_store.get(mission_start.mission_id)
    refreshed_segment = episode_store.get(mission_start.initial_episode_id)
    assert parent_final is not None
    assert parent_final["status"] == TaskCenterTaskStatus.DONE.value
    assert delegated_final is not None
    assert delegated_final.status == MissionStatus.SUCCEEDED
    assert refreshed_segment is not None
    assert refreshed_segment.status == EpisodeStatus.SUCCEEDED
