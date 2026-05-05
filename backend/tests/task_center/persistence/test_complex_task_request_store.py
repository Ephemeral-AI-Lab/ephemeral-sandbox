"""Persistence tests for ComplexTaskRequestStore."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.mission.mission import (
    ComplexTaskRequest,
    ComplexTaskRequestStatus,
)


def test_insert_returns_dto(request_store, task_center_run_id):
    req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    assert isinstance(req, ComplexTaskRequest)
    assert req.is_open
    assert req.task_segment_ids == ()


def test_get_round_trip(request_store, task_center_run_id):
    inserted = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    got = request_store.get(inserted.id)
    assert got is not None
    assert got.id == inserted.id
    assert got.goal == "g"
    assert got.requested_by_task_id == "t1"
    assert got.task_segment_ids == ()


def test_append_segment_id_persists_tuple(request_store, task_center_run_id):
    req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    after_first = request_store.append_segment_id(req.id, "s1")
    after_second = request_store.append_segment_id(req.id, "s2")
    assert after_first.task_segment_ids == ("s1",)
    assert after_second.task_segment_ids == ("s1", "s2")
    assert isinstance(after_second.task_segment_ids, tuple)


def test_set_status_records_outcome_and_closed_at(
    request_store, task_center_run_id
):
    req = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    closed_at = datetime.now(UTC)
    updated = request_store.set_status(
        req.id,
        status=ComplexTaskRequestStatus.SUCCEEDED,
        final_outcome={"outcome": "success"},
        closed_at=closed_at,
    )
    assert updated.status == ComplexTaskRequestStatus.SUCCEEDED
    assert updated.final_outcome == {"outcome": "success"}
    assert updated.closed_at is not None


def test_list_for_executor_task_orders_by_created_at(
    request_store, task_center_run_id
):
    a = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-A",
        goal="ga",
    )
    b = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-A",
        goal="gb",
    )
    request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="executor-B",
        goal="gc",
    )
    listed = request_store.list_for_executor_task("executor-A")
    assert [r.id for r in listed] == [a.id, b.id]
