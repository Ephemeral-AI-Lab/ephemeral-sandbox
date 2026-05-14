"""MissionHandler lifecycle tests covering Phase 01 exit criteria."""

from __future__ import annotations

import pytest

from task_center.config import TaskCenterLifecycleConfig
from task_center.mission.handler import MissionHandler
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.mission.state import MissionStatus
from task_center.episode.state import (
    AttemptPlanFailed,
    SuccessContinue,
    EpisodeClosureReport,
    TerminalSuccess,
)
from task_center.episode.state import (
    EpisodeCreationReason,
    EpisodeStatus,
)
from task_center.exceptions import TaskCenterInvariantViolation


@pytest.fixture
def handler(mission_store, episode_store, attempt_store):
    return MissionHandler(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        manager_registry=EpisodeManagerRegistry(),
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
    )


def test_create_mission_links_executor(
    handler, mission_store, task_center_run_id
):
    """Phase 01 exit: submit_execution_handoff -> request linked to requested_by_task_id."""
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="solve X",
    )
    assert req.requested_by_task_id == "executor-1"
    assert req.task_center_run_id == task_center_run_id
    assert req.is_open
    assert req.episode_ids == ()
    persisted = mission_store.get(req.id)
    assert persisted is not None
    assert persisted.requested_by_task_id == "executor-1"


def test_request_records_segments_in_episode_ids(
    handler, mission_store, task_center_run_id
):
    """Phase 01 exit: each request records created episodes in episode_ids."""
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    refreshed = mission_store.get(req.id)
    assert refreshed is not None
    assert refreshed.episode_ids == (seg.id,)


def test_initial_episode_has_sequence_one_and_initial_reason(handler, task_center_run_id):
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    assert seg.sequence_no == 1
    assert seg.creation_reason == EpisodeCreationReason.INITIAL
    assert seg.goal == "g"
    assert seg.is_open
    assert seg.attempt_budget == 2


def test_continuation_segment_inherits_continuation_goal(
    handler, episode_store, task_center_run_id
):
    """Phase 01 exit: continuation creates episode N+1 with goal from previous episode's continuation_goal."""
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="initial-goal",
    )
    seg1, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    # Mark predecessor SUCCEEDED with a continuation_goal so the invariant passes.
    episode_store.set_continuation_goal(seg1.id, "next-goal")
    episode_store.set_status(seg1.id, status=EpisodeStatus.SUCCEEDED)
    seg1_succeeded = episode_store.get(seg1.id)
    assert seg1_succeeded is not None

    seg2, _ = handler.create_continuation_episode_with_manager(
        previous_episode=seg1_succeeded
    )
    assert seg2.sequence_no == 2
    assert seg2.creation_reason == EpisodeCreationReason.PARTIAL_CONTINUATION
    assert seg2.goal == "next-goal"


def test_episode_ids_holds_multiple_segments(
    handler, mission_store, episode_store, task_center_run_id
):
    """Phase 01 exit: episode_ids can hold multiple Episode ids for one request."""
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g1",
    )
    seg1, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    episode_store.set_continuation_goal(seg1.id, "g2")
    episode_store.set_status(seg1.id, status=EpisodeStatus.SUCCEEDED)
    seg1_succeeded = episode_store.get(seg1.id)
    assert seg1_succeeded is not None
    seg2, _ = handler.create_continuation_episode_with_manager(
        previous_episode=seg1_succeeded
    )
    refreshed = mission_store.get(req.id)
    assert refreshed is not None
    assert refreshed.episode_ids == (seg1.id, seg2.id)


def test_handle_episode_closed_terminal_success_closes_request_succeeded(
    handler, mission_store, episode_store, task_center_run_id
):
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    handler.handle_episode_closed(
        EpisodeClosureReport(
            episode_id=seg.id,
            final_attempt_id="g1",
            outcome=TerminalSuccess(),
        )
    )
    final = mission_store.get(req.id)
    assert final is not None
    assert final.status == MissionStatus.SUCCEEDED
    assert final.final_outcome == {
        "outcome": "success",
        "final_episode_id": seg.id,
        "final_attempt_id": "g1",
    }


def test_handle_episode_closed_attempt_plan_failed_closes_request_failed(
    handler, mission_store, task_center_run_id
):
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    handler.handle_episode_closed(
        EpisodeClosureReport(
            episode_id=seg.id,
            final_attempt_id="g1",
            outcome=AttemptPlanFailed(
                failure_summary="boom", attempted_plan_history=()
            ),
        )
    )
    final = mission_store.get(req.id)
    assert final is not None
    assert final.status == MissionStatus.FAILED


def test_handle_episode_closed_success_continue_creates_continuation(
    handler, mission_store, episode_store, task_center_run_id
):
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg1, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    episode_store.set_continuation_goal(seg1.id, "next-goal")
    episode_store.set_status(seg1.id, status=EpisodeStatus.SUCCEEDED)
    handler.handle_episode_closed(
        EpisodeClosureReport(
            episode_id=seg1.id,
            final_attempt_id="g1",
            outcome=SuccessContinue(goal="next-goal"),
        )
    )
    refreshed = mission_store.get(req.id)
    assert refreshed is not None
    assert len(refreshed.episode_ids) == 2
    seg2_id = refreshed.episode_ids[1]
    seg2 = episode_store.get(seg2_id)
    assert seg2 is not None
    assert seg2.sequence_no == 2
    assert seg2.goal == "next-goal"


def test_handle_episode_closed_deregisters_manager(
    handler, task_center_run_id
):
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    # Access the registry through the handler's private attr for verification.
    reg = handler._manager_registry  # type: ignore[attr-defined]
    assert reg.get(seg.id) is not None
    handler.handle_episode_closed(
        EpisodeClosureReport(
            episode_id=seg.id,
            final_attempt_id="g1",
            outcome=TerminalSuccess(),
        )
    )
    assert reg.get(seg.id) is None


def test_continuation_segment_only_from_succeeded_predecessor_with_goal(
    handler, episode_store, task_center_run_id
):
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg1, _ = handler.create_initial_episode_with_manager(mission_id=req.id)

    # Predecessor still OPEN -> invariant violation.
    with pytest.raises(TaskCenterInvariantViolation):
        handler.create_continuation_episode_with_manager(previous_episode=seg1)

    # Predecessor SUCCEEDED but no continuation_goal -> invariant violation.
    episode_store.set_status(seg1.id, status=EpisodeStatus.SUCCEEDED)
    seg1_no_goal = episode_store.get(seg1.id)
    assert seg1_no_goal is not None
    with pytest.raises(TaskCenterInvariantViolation):
        handler.create_continuation_episode_with_manager(previous_episode=seg1_no_goal)


def test_episode_manager_registry_enforces_unique_per_segment(
    handler, task_center_run_id
):
    """Phase 01 spec: exactly one EpisodeManager active per open episode."""
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    handler.create_initial_episode_with_manager(mission_id=req.id)
    # Calling create_initial_episode again should fail because the request now
    # has episode 1 — sequence_no 1 is no longer the contiguous next.
    with pytest.raises(TaskCenterInvariantViolation):
        handler.create_initial_episode_with_manager(mission_id=req.id)


def test_close_mission_delivers_closure_report_when_callback_set(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    delivered: list = []

    def sink(report) -> None:
        delivered.append(report)

    handler = MissionHandler(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        manager_registry=EpisodeManagerRegistry(),
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
        deliver_closure_report=sink,
    )
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    handler.create_initial_episode_with_manager(mission_id=req.id)
    handler.close_mission(
        mission_id=req.id,
        succeeded=True,
        final_episode_id="seg",
        final_attempt_id="g1",
    )
    assert len(delivered) == 1
    assert delivered[0].outcome == "success"
    assert delivered[0].requested_by_task_id == "executor-1"


def test_handler_passes_orchestrator_factory_to_spawned_manager(
    mission_store, episode_store, attempt_store, task_center_run_id
):
    started: list[str] = []

    class _StartedOrchestrator:
        def __init__(self, attempt_id: str) -> None:
            self.attempt_id = attempt_id

        def start(self) -> None:
            started.append(self.attempt_id)

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id)

    registry = EpisodeManagerRegistry()
    handler = MissionHandler(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        manager_registry=registry,
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
        orchestrator_factory=factory,
    )
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    episode, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    manager = registry.get(episode.id)
    assert manager is not None

    attempt = manager.create_initial_attempt()

    assert started == [attempt.id]


def test_no_root_creation_reason_in_lifecycle(handler, task_center_run_id):
    """Phase 01 spec: 'root' creation reason is not allowed."""
    # Indirect: handler-driven episode creation only ever uses INITIAL or
    # PARTIAL_CONTINUATION. There is no public path that produces 'root'.
    req = handler.create_mission(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg, _ = handler.create_initial_episode_with_manager(mission_id=req.id)
    assert seg.creation_reason in (
        EpisodeCreationReason.INITIAL,
        EpisodeCreationReason.PARTIAL_CONTINUATION,
    )
