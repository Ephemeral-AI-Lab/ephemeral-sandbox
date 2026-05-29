"""Persistence tests for IterationStore."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.iteration.state import (
    Iteration,
    IterationCreationReason,
    IterationStatus,
)


def _seed_request(workflow_store, task_center_run_id) -> str:
    req = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    return req.id


def test_insert_returns_dto(iteration_store, workflow_store, task_center_run_id):
    request_id = _seed_request(workflow_store, task_center_run_id)
    seg = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    assert isinstance(seg, Iteration)
    assert seg.is_open
    assert seg.attempt_ids == ()
    assert seg.attempt_budget == 2


def test_get_round_trip(iteration_store, workflow_store, task_center_run_id):
    request_id = _seed_request(workflow_store, task_center_run_id)
    inserted = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    got = iteration_store.get(inserted.id)
    assert got is not None
    assert got.id == inserted.id
    assert got.creation_reason == IterationCreationReason.INITIAL


def test_append_attempt_id_preserves_order(
    iteration_store, workflow_store, task_center_run_id
):
    request_id = _seed_request(workflow_store, task_center_run_id)
    seg = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=3,
    )
    s1 = iteration_store.append_attempt_id(seg.id, "g1")
    s2 = iteration_store.append_attempt_id(seg.id, "g2")
    assert s1.attempt_ids == ("g1",)
    assert s2.attempt_ids == ("g1", "g2")
    assert s2.attempt_count == 2


def test_deferred_goal_dto_field_maps_to_deferred_goal_db_column(
    iteration_store, workflow_store, task_center_run_id
):
    """Store seam translates DTO field `deferred_goal_for_next_iteration`
    to DB column `deferred_goal`. Raw-SQL queries must target the column name,
    not the DTO name.
    """
    from db.models.iteration import IterationRecord

    request_id = _seed_request(workflow_store, task_center_run_id)
    seg = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    seg = iteration_store.set_deferred_goal_for_next_iteration(seg.id, "deferred-scope")
    assert seg.deferred_goal_for_next_iteration == "deferred-scope"

    with iteration_store._sf() as db:  # noqa: SLF001
        record = db.get(IterationRecord, seg.id)
        assert record.deferred_goal == "deferred-scope"
        assert not hasattr(IterationRecord, "deferred_goal_for_next_iteration")


def test_set_deferred_goal_for_next_iteration_and_status(
    iteration_store, workflow_store, task_center_run_id
):
    request_id = _seed_request(workflow_store, task_center_run_id)
    seg = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    seg = iteration_store.set_deferred_goal_for_next_iteration(seg.id, "next-goal")
    assert seg.deferred_goal_for_next_iteration == "next-goal"
    seg = iteration_store.set_status(
        seg.id,
        status=IterationStatus.SUCCEEDED,
        closed_at=datetime.now(UTC),
    )
    assert seg.status == IterationStatus.SUCCEEDED
    assert seg.closed_at is not None


def test_list_for_goal_orders_by_sequence_no(
    iteration_store, workflow_store, task_center_run_id
):
    request_id = _seed_request(workflow_store, task_center_run_id)
    s2 = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=2,
        creation_reason=IterationCreationReason.DEFERRED_GOAL_CONTINUATION,
        goal="g2",
        attempt_budget=2,
    )
    s1 = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g1",
        attempt_budget=2,
    )
    listed = iteration_store.list_for_workflow(request_id)
    assert [s.id for s in listed] == [s1.id, s2.id]


def test_get_by_sequence(iteration_store, workflow_store, task_center_run_id):
    request_id = _seed_request(workflow_store, task_center_run_id)
    seg = iteration_store.insert(
        workflow_id=request_id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    found = iteration_store.get_by_sequence(
        workflow_id=request_id, sequence_no=1
    )
    assert found is not None
    assert found.id == seg.id
    missing = iteration_store.get_by_sequence(
        workflow_id=request_id, sequence_no=99
    )
    assert missing is None
