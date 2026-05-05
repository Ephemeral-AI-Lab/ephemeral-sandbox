"""Invariant tests across request, segment, and graph levels."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.mission.validation import (
    assert_continuation_episode_predecessor,
    assert_mission_request_open,
    assert_episode_id_unique_in_mission,
    assert_episode_sequence_contiguous,
)
from task_center.attempt.validation import (
    assert_fail_reason_present_on_failure,
    assert_graph_sequence_contiguous,
)
from task_center.episode.validation import (
    assert_attempt_belongs_to_episode,
    assert_episode_has_budget,
    assert_episode_open,
)
from task_center.episode.registry import SegmentManagerRegistry
from task_center.mission.mission import (
    ComplexTaskRequest,
    ComplexTaskRequestStatus,
)
from task_center.attempt import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)
from task_center.episode.episode import (
    TaskSegment,
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)
from task_center.exceptions import GraphInvariantViolation


def _request(
    status: ComplexTaskRequestStatus = ComplexTaskRequestStatus.OPEN,
    task_segment_ids: tuple[str, ...] = (),
) -> ComplexTaskRequest:
    now = datetime.now(UTC)
    return ComplexTaskRequest(
        id="r1",
        task_center_run_id="run1",
        requested_by_task_id="t1",
        goal="g",
        status=status,
        task_segment_ids=task_segment_ids,
        final_outcome=None,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


def _segment(
    *,
    status: TaskSegmentStatus = TaskSegmentStatus.OPEN,
    harness_graph_ids: tuple[str, ...] = (),
    continuation_goal: str | None = None,
    attempt_budget: int = 2,
    sid: str = "s1",
) -> TaskSegment:
    now = datetime.now(UTC)
    return TaskSegment(
        id=sid,
        complex_task_request_id="r1",
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=attempt_budget,
        status=status,
        harness_graph_ids=harness_graph_ids,
        continuation_goal=continuation_goal,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


def _graph(
    *,
    status: HarnessGraphStatus = HarnessGraphStatus.RUNNING,
    fail_reason: HarnessGraphFailReason | None = None,
    task_segment_id: str = "s1",
    gid: str = "g1",
) -> HarnessGraph:
    now = datetime.now(UTC)
    return HarnessGraph(
        id=gid,
        task_segment_id=task_segment_id,
        graph_sequence_no=1,
        stage=HarnessGraphStage.PLANNING,
        status=status,
        planner_task_id=None,
        task_specification=None,
        evaluation_criteria=(),
        generator_task_ids=(),
        evaluator_task_id=None,
        continuation_goal=None,
        fail_reason=fail_reason,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


# ---- Request-level ------------------------------------------------------


def test_assert_mission_request_open_passes_for_open():
    assert_mission_request_open(_request(status=ComplexTaskRequestStatus.OPEN))


def test_assert_mission_request_open_fails_for_closed():
    for status in (
        ComplexTaskRequestStatus.SUCCEEDED,
        ComplexTaskRequestStatus.FAILED,
        ComplexTaskRequestStatus.CANCELLED,
    ):
        with pytest.raises(GraphInvariantViolation):
            assert_mission_request_open(_request(status=status))


def test_assert_episode_id_unique_in_mission():
    assert_episode_id_unique_in_mission(
        _request(task_segment_ids=("s1", "s2")), "s3"
    )
    with pytest.raises(GraphInvariantViolation):
        assert_episode_id_unique_in_mission(
            _request(task_segment_ids=("s1",)), "s1"
        )


def test_assert_episode_sequence_contiguous():
    assert_episode_sequence_contiguous(_request(task_segment_ids=()), 1)
    assert_episode_sequence_contiguous(_request(task_segment_ids=("s1",)), 2)
    with pytest.raises(GraphInvariantViolation):
        assert_episode_sequence_contiguous(_request(task_segment_ids=("s1",)), 1)
    with pytest.raises(GraphInvariantViolation):
        assert_episode_sequence_contiguous(_request(task_segment_ids=("s1",)), 3)


def test_assert_continuation_episode_predecessor_requires_succeeded_with_goal():
    succeeded_with_goal = _segment(
        status=TaskSegmentStatus.SUCCEEDED, continuation_goal="next"
    )
    assert_continuation_episode_predecessor(succeeded_with_goal)

    with pytest.raises(GraphInvariantViolation):
        assert_continuation_episode_predecessor(
            _segment(status=TaskSegmentStatus.OPEN, continuation_goal="next")
        )
    with pytest.raises(GraphInvariantViolation):
        assert_continuation_episode_predecessor(
            _segment(status=TaskSegmentStatus.SUCCEEDED, continuation_goal=None)
        )


# ---- Segment-level ------------------------------------------------------


def test_assert_episode_open():
    assert_episode_open(_segment(status=TaskSegmentStatus.OPEN))
    with pytest.raises(GraphInvariantViolation):
        assert_episode_open(_segment(status=TaskSegmentStatus.SUCCEEDED))


def test_assert_episode_has_budget():
    assert_episode_has_budget(_segment(attempt_budget=2, harness_graph_ids=()))
    assert_episode_has_budget(
        _segment(attempt_budget=2, harness_graph_ids=("g1",))
    )
    with pytest.raises(GraphInvariantViolation):
        assert_episode_has_budget(
            _segment(attempt_budget=2, harness_graph_ids=("g1", "g2"))
        )


def test_assert_attempt_belongs_to_episode():
    assert_attempt_belongs_to_episode(
        _graph(task_segment_id="s1"), _segment(sid="s1")
    )
    with pytest.raises(GraphInvariantViolation):
        assert_attempt_belongs_to_episode(
            _graph(task_segment_id="s1"), _segment(sid="s2")
        )


# ---- Graph-level --------------------------------------------------------


def test_assert_graph_sequence_contiguous():
    assert_graph_sequence_contiguous(_segment(harness_graph_ids=()), 1)
    assert_graph_sequence_contiguous(_segment(harness_graph_ids=("g1",)), 2)
    with pytest.raises(GraphInvariantViolation):
        assert_graph_sequence_contiguous(_segment(harness_graph_ids=("g1",)), 1)


def test_assert_fail_reason_present_on_failure():
    assert_fail_reason_present_on_failure(
        _graph(status=HarnessGraphStatus.PASSED)
    )
    assert_fail_reason_present_on_failure(
        _graph(
            status=HarnessGraphStatus.FAILED,
            fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
        )
    )
    with pytest.raises(GraphInvariantViolation):
        assert_fail_reason_present_on_failure(
            _graph(status=HarnessGraphStatus.FAILED, fail_reason=None)
        )


# ---- Manager registry ---------------------------------------------------


def test_segment_manager_registry_enforces_uniqueness():
    reg = SegmentManagerRegistry()

    class _Fake:
        task_segment_id = "s1"

    reg.register(_Fake())  # type: ignore[arg-type]
    assert reg.get("s1") is not None
    with pytest.raises(GraphInvariantViolation):
        reg.register(_Fake())  # type: ignore[arg-type]
    reg.deregister("s1")
    assert reg.get("s1") is None
