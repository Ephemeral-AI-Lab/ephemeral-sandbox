"""Persistence tests for HarnessGraphStore."""

from __future__ import annotations

from task_center.attempt import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)
from task_center.episode.episode import TaskSegmentCreationReason


def _seed_segment(
    request_store, segment_store, task_center_run_id, sequence_no=1
) -> str:
    req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = segment_store.insert(
        complex_task_request_id=req.id,
        sequence_no=sequence_no,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    return seg.id


def test_insert_returns_running_planning_dto(
    graph_store, segment_store, request_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    g = graph_store.insert(task_segment_id=seg_id, graph_sequence_no=1)
    assert isinstance(g, HarnessGraph)
    assert g.stage == HarnessGraphStage.PLANNING
    assert g.status == HarnessGraphStatus.RUNNING
    assert g.evaluation_criteria == ()
    assert g.generator_task_ids == ()
    assert g.fail_reason is None


def test_set_plan_contract_persists_fields(
    graph_store, segment_store, request_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    g = graph_store.insert(task_segment_id=seg_id, graph_sequence_no=1)
    g = graph_store.set_plan_contract(
        g.id,
        task_specification="spec",
        evaluation_criteria=["c1", "c2"],
        continuation_goal="next",
    )
    assert g.task_specification == "spec"
    assert g.evaluation_criteria == ("c1", "c2")
    assert g.continuation_goal == "next"


def test_close_records_status_fail_reason_and_closed_at(
    graph_store, segment_store, request_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    g = graph_store.insert(task_segment_id=seg_id, graph_sequence_no=1)
    closed = graph_store.close(
        g.id,
        status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.EVALUATOR_FAILED,
    )
    assert closed.is_closed
    assert closed.status == HarnessGraphStatus.FAILED
    assert closed.fail_reason == HarnessGraphFailReason.EVALUATOR_FAILED
    assert closed.closed_at is not None


def test_list_for_segment_orders_by_graph_sequence_no(
    graph_store, segment_store, request_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    g2 = graph_store.insert(task_segment_id=seg_id, graph_sequence_no=2)
    g1 = graph_store.insert(task_segment_id=seg_id, graph_sequence_no=1)
    listed = graph_store.list_for_segment(seg_id)
    assert [g.id for g in listed] == [g1.id, g2.id]


def test_get_by_sequence(
    graph_store, segment_store, request_store, task_center_run_id
):
    seg_id = _seed_segment(request_store, segment_store, task_center_run_id)
    g = graph_store.insert(task_segment_id=seg_id, graph_sequence_no=1)
    found = graph_store.get_by_sequence(
        task_segment_id=seg_id, graph_sequence_no=1
    )
    assert found is not None and found.id == g.id
    missing = graph_store.get_by_sequence(
        task_segment_id=seg_id, graph_sequence_no=99
    )
    assert missing is None
