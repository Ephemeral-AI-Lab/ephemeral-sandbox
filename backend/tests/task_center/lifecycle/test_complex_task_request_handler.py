"""ComplexTaskRequestHandler lifecycle tests covering Phase 01 exit criteria."""

from __future__ import annotations

import pytest

from task_center.config import HarnessLifecycleConfig
from task_center.mission.handler import ComplexTaskRequestHandler
from task_center.episode.registry import SegmentManagerRegistry
from task_center.mission.mission import ComplexTaskRequestStatus
from task_center.episode.closure_report import (
    AttemptPlanFailed,
    SuccessContinue,
    TaskSegmentClosureReport,
    TerminalSuccess,
)
from task_center.episode.episode import (
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)
from task_center.exceptions import GraphInvariantViolation


@pytest.fixture
def handler(request_store, segment_store, graph_store):
    return ComplexTaskRequestHandler(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        manager_registry=SegmentManagerRegistry(),
        config=HarnessLifecycleConfig(default_attempt_budget=2),
    )


def test_create_mission_request_links_executor(
    handler, request_store, task_center_run_id
):
    """Phase 01 exit: request_complex_task_solution -> request linked to requested_by_task_id."""
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="solve X",
    )
    assert req.requested_by_task_id == "executor-1"
    assert req.task_center_run_id == task_center_run_id
    assert req.is_open
    assert req.task_segment_ids == ()
    persisted = request_store.get(req.id)
    assert persisted is not None
    assert persisted.requested_by_task_id == "executor-1"


def test_request_records_segments_in_task_segment_ids(
    handler, request_store, task_center_run_id
):
    """Phase 01 exit: each request records created segments in task_segment_ids."""
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    refreshed = request_store.get(req.id)
    assert refreshed is not None
    assert refreshed.task_segment_ids == (seg.id,)


def test_initial_segment_has_sequence_one_and_initial_reason(handler, task_center_run_id):
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    assert seg.sequence_no == 1
    assert seg.creation_reason == TaskSegmentCreationReason.INITIAL
    assert seg.goal == "g"
    assert seg.is_open
    assert seg.attempt_budget == 2


def test_continuation_segment_inherits_continuation_goal(
    handler, segment_store, task_center_run_id
):
    """Phase 01 exit: continuation creates segment N+1 with goal from previous segment's continuation_goal."""
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="initial-goal",
    )
    seg1 = handler.create_initial_episode(complex_task_request_id=req.id)
    # Mark predecessor SUCCEEDED with a continuation_goal so the invariant passes.
    segment_store.set_continuation_goal(seg1.id, "next-goal")
    segment_store.set_status(seg1.id, status=TaskSegmentStatus.SUCCEEDED)
    seg1_succeeded = segment_store.get(seg1.id)
    assert seg1_succeeded is not None

    seg2 = handler.create_continuation_episode(previous_segment=seg1_succeeded)
    assert seg2.sequence_no == 2
    assert seg2.creation_reason == TaskSegmentCreationReason.PARTIAL_CONTINUATION
    assert seg2.goal == "next-goal"


def test_task_segment_ids_holds_multiple_segments(
    handler, request_store, segment_store, task_center_run_id
):
    """Phase 01 exit: task_segment_ids can hold multiple TaskSegment ids for one request."""
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g1",
    )
    seg1 = handler.create_initial_episode(complex_task_request_id=req.id)
    segment_store.set_continuation_goal(seg1.id, "g2")
    segment_store.set_status(seg1.id, status=TaskSegmentStatus.SUCCEEDED)
    seg1_succeeded = segment_store.get(seg1.id)
    assert seg1_succeeded is not None
    seg2 = handler.create_continuation_episode(previous_segment=seg1_succeeded)
    refreshed = request_store.get(req.id)
    assert refreshed is not None
    assert refreshed.task_segment_ids == (seg1.id, seg2.id)


def test_handle_episode_closed_terminal_success_closes_request_succeeded(
    handler, request_store, segment_store, task_center_run_id
):
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    handler.handle_episode_closed(
        TaskSegmentClosureReport(
            task_segment_id=seg.id,
            final_harness_graph_id="g1",
            outcome=TerminalSuccess(),
        )
    )
    final = request_store.get(req.id)
    assert final is not None
    assert final.status == ComplexTaskRequestStatus.SUCCEEDED
    assert final.final_outcome == {
        "outcome": "success",
        "final_segment_id": seg.id,
        "final_harness_graph_id": "g1",
    }


def test_handle_episode_closed_attempt_plan_failed_closes_request_failed(
    handler, request_store, task_center_run_id
):
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    handler.handle_episode_closed(
        TaskSegmentClosureReport(
            task_segment_id=seg.id,
            final_harness_graph_id="g1",
            outcome=AttemptPlanFailed(
                failure_summary="boom", attempted_plan_history=()
            ),
        )
    )
    final = request_store.get(req.id)
    assert final is not None
    assert final.status == ComplexTaskRequestStatus.FAILED


def test_handle_episode_closed_success_continue_creates_continuation(
    handler, request_store, segment_store, task_center_run_id
):
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg1 = handler.create_initial_episode(complex_task_request_id=req.id)
    segment_store.set_continuation_goal(seg1.id, "next-goal")
    segment_store.set_status(seg1.id, status=TaskSegmentStatus.SUCCEEDED)
    handler.handle_episode_closed(
        TaskSegmentClosureReport(
            task_segment_id=seg1.id,
            final_harness_graph_id="g1",
            outcome=SuccessContinue(goal="next-goal"),
        )
    )
    refreshed = request_store.get(req.id)
    assert refreshed is not None
    assert len(refreshed.task_segment_ids) == 2
    seg2_id = refreshed.task_segment_ids[1]
    seg2 = segment_store.get(seg2_id)
    assert seg2 is not None
    assert seg2.sequence_no == 2
    assert seg2.goal == "next-goal"


def test_handle_episode_closed_deregisters_manager(
    handler, task_center_run_id
):
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    # Access the registry through the handler's private attr for verification.
    reg = handler._manager_registry  # type: ignore[attr-defined]
    assert reg.get(seg.id) is not None
    handler.handle_episode_closed(
        TaskSegmentClosureReport(
            task_segment_id=seg.id,
            final_harness_graph_id="g1",
            outcome=TerminalSuccess(),
        )
    )
    assert reg.get(seg.id) is None


def test_continuation_segment_only_from_succeeded_predecessor_with_goal(
    handler, segment_store, task_center_run_id
):
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg1 = handler.create_initial_episode(complex_task_request_id=req.id)

    # Predecessor still OPEN -> invariant violation.
    with pytest.raises(GraphInvariantViolation):
        handler.create_continuation_episode(previous_segment=seg1)

    # Predecessor SUCCEEDED but no continuation_goal -> invariant violation.
    segment_store.set_status(seg1.id, status=TaskSegmentStatus.SUCCEEDED)
    seg1_no_goal = segment_store.get(seg1.id)
    assert seg1_no_goal is not None
    with pytest.raises(GraphInvariantViolation):
        handler.create_continuation_episode(previous_segment=seg1_no_goal)


def test_segment_manager_registry_enforces_unique_per_segment(
    handler, task_center_run_id
):
    """Phase 01 spec: exactly one TaskSegmentManager active per open segment."""
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    handler.create_initial_episode(complex_task_request_id=req.id)
    # Calling create_initial_episode again should fail because the request now
    # has segment 1 — sequence_no 1 is no longer the contiguous next.
    with pytest.raises(GraphInvariantViolation):
        handler.create_initial_episode(complex_task_request_id=req.id)


def test_close_mission_request_delivers_close_report_when_callback_set(
    request_store, segment_store, graph_store, task_center_run_id
):
    delivered: list = []

    def sink(report) -> None:
        delivered.append(report)

    handler = ComplexTaskRequestHandler(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        manager_registry=SegmentManagerRegistry(),
        config=HarnessLifecycleConfig(default_attempt_budget=2),
        deliver_close_report=sink,
    )
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    handler.create_initial_episode(complex_task_request_id=req.id)
    handler.close_mission_request(
        complex_task_request_id=req.id,
        succeeded=True,
        final_segment_id="seg",
        final_harness_graph_id="g1",
    )
    assert len(delivered) == 1
    assert delivered[0].outcome == "success"
    assert delivered[0].requested_by_task_id == "executor-1"


def test_handler_passes_orchestrator_factory_to_spawned_manager(
    request_store, segment_store, graph_store, task_center_run_id
):
    started: list[str] = []

    class _StartedOrchestrator:
        def __init__(self, graph_id: str) -> None:
            self.harness_graph_id = graph_id

        def start(self) -> None:
            started.append(self.harness_graph_id)

    def factory(graph, on_graph_closed):
        del on_graph_closed
        return _StartedOrchestrator(graph.id)

    registry = SegmentManagerRegistry()
    handler = ComplexTaskRequestHandler(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        manager_registry=registry,
        config=HarnessLifecycleConfig(default_attempt_budget=2),
        orchestrator_factory=factory,
    )
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-1",
        goal="g",
    )
    segment = handler.create_initial_episode(complex_task_request_id=req.id)
    manager = registry.get(segment.id)
    assert manager is not None

    graph = manager.create_initial_attempt()

    assert started == [graph.id]


def test_no_root_creation_reason_in_lifecycle(handler, task_center_run_id):
    """Phase 01 spec: 'root' creation reason is not allowed."""
    # Indirect: handler-driven segment creation only ever uses INITIAL or
    # PARTIAL_CONTINUATION. There is no public path that produces 'root'.
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    assert seg.creation_reason in (
        TaskSegmentCreationReason.INITIAL,
        TaskSegmentCreationReason.PARTIAL_CONTINUATION,
    )
