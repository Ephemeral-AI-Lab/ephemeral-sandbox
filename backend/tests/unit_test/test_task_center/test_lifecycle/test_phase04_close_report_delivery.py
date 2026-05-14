"""Phase 04 close-report router tests."""

from __future__ import annotations

import pytest

from task_center.mission.close_report_router import (
    MissionClosureReportRouter,
)
from task_center.mission.state import MissionClosureReport
from task_center._core.types import TaskCenterInvariantViolation
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, AttemptDeps
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.episode.state import EpisodeCreationReason
from task_center.task_state import TaskCenterTaskStatus, PlannedGeneratorTask, PlannerSubmission
from task_center._core.types import generator_task_id


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def _build_runtime_with_open_graph(
    *,
    mission_store,
    episode_store,
    attempt_store,
    task_store,
    task_center_run_id: str,
    composer,
):
    request = mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="root",
        goal="outer",
    )
    episode = episode_store.insert(
        mission_id=request.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="outer",
        attempt_budget=2,
    )
    mission_store.append_episode_id(request.id, episode.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    episode_store.append_attempt_id(episode.id, attempt.id)
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=registry,
        manager_registry=EpisodeManagerRegistry(),
        composer=composer,
    )
    orchestrator = AttemptOrchestrator(
        attempt=attempt,
        on_attempt_closed=lambda attempt_id: None,
        runtime=runtime,
    )
    registry.register(orchestrator)
    orchestrator.start()
    orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=attempt.id,
            planner_task_id=f"{attempt.id}:planner",
            kind="full",
            task_specification="spec",
            evaluation_criteria=("c",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="a",
                    agent_name="executor",
                    deps=(),
                    task_spec="do",
                ),
            ),
            continuation_goal=None,
            summary="plan",
        )
    )
    parent_task_id = generator_task_id(attempt.id, "a")
    return runtime, attempt.id, parent_task_id


def _set_parent_waiting(task_store, parent_task_id: str) -> None:
    task_store.set_task_status(
        parent_task_id,
        status=TaskCenterTaskStatus.WAITING_MISSION.value,
    )


def test_router_delivers_success_to_waiting_parent(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, parent_attempt_id, parent_task_id = _build_runtime_with_open_graph(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    _set_parent_waiting(task_store, parent_task_id)
    router = MissionClosureReportRouter(runtime=runtime)

    result = router.deliver(
        MissionClosureReport(
            mission_id="delegated-1",
            requested_by_task_id=parent_task_id,
            outcome="success",
            final_episode_id="seg-1",
            final_attempt_id="attempt-1",
        )
    )

    assert result.status == "delivered"
    assert result.parent_attempt_id == parent_attempt_id
    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.DONE.value


def test_router_delivers_failure_marks_parent_failed_and_blocks_dependents(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    request = mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="root",
        goal="outer",
    )
    episode = episode_store.insert(
        mission_id=request.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="outer",
        attempt_budget=2,
    )
    mission_store.append_episode_id(request.id, episode.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    episode_store.append_attempt_id(episode.id, attempt.id)
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=registry,
        manager_registry=EpisodeManagerRegistry(),
        composer=composer,
    )
    orchestrator = AttemptOrchestrator(
        attempt=attempt,
        on_attempt_closed=lambda attempt_id: None,
        runtime=runtime,
    )
    registry.register(orchestrator)
    orchestrator.start()
    orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=attempt.id,
            planner_task_id=f"{attempt.id}:planner",
            kind="full",
            task_specification="spec",
            evaluation_criteria=("c",),
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
            ),
            continuation_goal=None,
            summary="plan",
        )
    )
    parent_task_id = generator_task_id(attempt.id, "a")
    dependent_id = generator_task_id(attempt.id, "b")
    _set_parent_waiting(task_store, parent_task_id)
    router = MissionClosureReportRouter(runtime=runtime)

    result = router.deliver(
        MissionClosureReport(
            mission_id="delegated-1",
            requested_by_task_id=parent_task_id,
            outcome="failed",
            final_episode_id="seg-1",
            final_attempt_id="attempt-1",
        )
    )

    assert result.status == "delivered"
    parent_task = task_store.get_task(parent_task_id)
    dependent = task_store.get_task(dependent_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.FAILED.value
    assert dependent is not None
    assert dependent["status"] == TaskCenterTaskStatus.BLOCKED.value


def test_router_treats_done_parent_as_already_delivered(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    task_store.set_task_status(
        parent_task_id, status=TaskCenterTaskStatus.DONE.value
    )
    router = MissionClosureReportRouter(runtime=runtime)

    result = router.deliver(
        MissionClosureReport(
            mission_id="delegated-1",
            requested_by_task_id=parent_task_id,
            outcome="success",
            final_episode_id="seg-1",
            final_attempt_id="attempt-1",
        )
    )

    assert result.status == "already_delivered"


def test_router_raises_when_parent_orchestrator_missing(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    """No-restart invariant: while a parent task is in WAITING_MISSION
    its orchestrator must remain registered. A missing orchestrator at
    delivery time is a hard ``TaskCenterInvariantViolation``."""
    runtime, parent_attempt_id, parent_task_id = _build_runtime_with_open_graph(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    _set_parent_waiting(task_store, parent_task_id)
    runtime.orchestrator_registry.deregister(parent_attempt_id)
    router = MissionClosureReportRouter(runtime=runtime)

    with pytest.raises(TaskCenterInvariantViolation):
        router.deliver(
            MissionClosureReport(
                mission_id="delegated-1",
                requested_by_task_id=parent_task_id,
                outcome="success",
                final_episode_id="seg-1",
                final_attempt_id="attempt-1",
            )
        )

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.WAITING_MISSION.value


def test_router_rejects_running_parent(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    # Parent is RUNNING (not waiting) — illegal report state.
    router = MissionClosureReportRouter(runtime=runtime)

    with pytest.raises(TaskCenterInvariantViolation):
        router.deliver(
            MissionClosureReport(
                mission_id="delegated-1",
                requested_by_task_id=parent_task_id,
                outcome="success",
                final_episode_id="seg-1",
                final_attempt_id="attempt-1",
            )
        )


def test_apply_closure_report_is_idempotent_on_second_delivery(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    _set_parent_waiting(task_store, parent_task_id)
    parent_task_before = task_store.get_task(parent_task_id)
    assert parent_task_before is not None
    summary_count_before = len(parent_task_before["summaries"])

    report = MissionClosureReport(
        mission_id="delegated-1",
        requested_by_task_id=parent_task_id,
        outcome="success",
        final_episode_id="seg-1",
        final_attempt_id="attempt-1",
    )
    # Find the orchestrator and apply the close report twice. Second call
    # must be silently idempotent (CAS miss).
    parent_attempt_id = parent_task_before["task_center_attempt_id"]
    orchestrator = runtime.orchestrator_registry.get_or_raise(parent_attempt_id)
    orchestrator.apply_mission_closure_report(report)
    orchestrator.apply_mission_closure_report(report)

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.DONE.value
    # Exactly one new summary appended.
    assert len(parent_task["summaries"]) == summary_count_before + 1


def test_router_routes_entry_mode_closure_report_through_controller(
    mission_store, episode_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    """Entry-mode close-report dispatch.

    When the parent task has ``task_center_attempt_id=None``, the
    router must look up :attr:`AttemptDeps.entry_task_controller`
    instead of the orchestrator registry, and route the close report into
    the controller's ``apply_mission_closure_report``.
    """
    from task_center.entry.controller import EntryTaskController
    from task_center.task_state import TaskCenterTaskRole

    # Seed entry-mode caller in WAITING_MISSION.
    entry_task_id = "entry-task"
    task_store.upsert_task(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        role=TaskCenterTaskRole.GENERATOR.value,
        agent_name="entry_executor",
        rendered_prompt="entry goal",
        status=TaskCenterTaskStatus.WAITING_MISSION.value,
        summaries=[],
        needs=[],
        task_center_attempt_id=None,
        spawn_reason="entry_executor",
    )
    controller = EntryTaskController(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        task_store=task_store,
    )
    runtime = AttemptDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=AttemptOrchestratorRegistry(),
        manager_registry=EpisodeManagerRegistry(),
        composer=composer,
        entry_task_controller=controller,
    )

    router = MissionClosureReportRouter(runtime=runtime)
    result = router.deliver(
        MissionClosureReport(
            mission_id="delegated-x",
            requested_by_task_id=entry_task_id,
            outcome="success",
            final_episode_id="delegated-seg",
            final_attempt_id="delegated-attempt",
        )
    )

    assert result.status == "delivered"
    assert result.parent_attempt_id is None
    entry_task = task_store.get_task(entry_task_id)
    run = task_store.get_run(task_center_run_id)
    assert entry_task is not None
    assert entry_task["status"] == TaskCenterTaskStatus.DONE.value
    assert run is not None
    assert run["status"] == "done"
