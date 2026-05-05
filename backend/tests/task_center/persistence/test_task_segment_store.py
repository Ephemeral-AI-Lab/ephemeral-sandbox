"""Persistence tests for TaskSegmentStore."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.episode.episode import (
    TaskSegment,
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)


def _seed_request(request_store, task_center_run_id) -> str:
    req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    return req.id


def test_insert_returns_dto(segment_store, request_store, task_center_run_id):
    request_id = _seed_request(request_store, task_center_run_id)
    seg = segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    assert isinstance(seg, TaskSegment)
    assert seg.is_open
    assert seg.harness_graph_ids == ()
    assert seg.attempt_budget == 2


def test_get_round_trip(segment_store, request_store, task_center_run_id):
    request_id = _seed_request(request_store, task_center_run_id)
    inserted = segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    got = segment_store.get(inserted.id)
    assert got is not None
    assert got.id == inserted.id
    assert got.creation_reason == TaskSegmentCreationReason.INITIAL


def test_append_graph_id_preserves_order(
    segment_store, request_store, task_center_run_id
):
    request_id = _seed_request(request_store, task_center_run_id)
    seg = segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=3,
    )
    s1 = segment_store.append_graph_id(seg.id, "g1")
    s2 = segment_store.append_graph_id(seg.id, "g2")
    assert s1.harness_graph_ids == ("g1",)
    assert s2.harness_graph_ids == ("g1", "g2")
    assert s2.attempt_count == 2


def test_set_continuation_goal_and_status(
    segment_store, request_store, task_center_run_id
):
    request_id = _seed_request(request_store, task_center_run_id)
    seg = segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    seg = segment_store.set_continuation_goal(seg.id, "next-goal")
    assert seg.continuation_goal == "next-goal"
    seg = segment_store.set_status(
        seg.id,
        status=TaskSegmentStatus.SUCCEEDED,
        closed_at=datetime.now(UTC),
    )
    assert seg.status == TaskSegmentStatus.SUCCEEDED
    assert seg.closed_at is not None


def test_list_for_request_orders_by_sequence_no(
    segment_store, request_store, task_center_run_id
):
    request_id = _seed_request(request_store, task_center_run_id)
    s2 = segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=2,
        creation_reason=TaskSegmentCreationReason.PARTIAL_CONTINUATION,
        goal="g2",
        attempt_budget=2,
    )
    s1 = segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g1",
        attempt_budget=2,
    )
    listed = segment_store.list_for_request(request_id)
    assert [s.id for s in listed] == [s1.id, s2.id]


def test_get_by_sequence(segment_store, request_store, task_center_run_id):
    request_id = _seed_request(request_store, task_center_run_id)
    seg = segment_store.insert(
        complex_task_request_id=request_id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    found = segment_store.get_by_sequence(
        complex_task_request_id=request_id, sequence_no=1
    )
    assert found is not None
    assert found.id == seg.id
    missing = segment_store.get_by_sequence(
        complex_task_request_id=request_id, sequence_no=99
    )
    assert missing is None
