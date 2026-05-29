"""Persistence tests for WorkflowStore."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.workflow.state import (
    Workflow,
    WorkflowStatus,
)


def test_insert_returns_dto(workflow_store, task_center_run_id):
    req = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    assert isinstance(req, Workflow)
    assert req.is_open
    assert req.iteration_ids == ()


def test_get_round_trip(workflow_store, task_center_run_id):
    inserted = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    got = workflow_store.get(inserted.id)
    assert got is not None
    assert got.id == inserted.id
    assert got.goal == "g"
    assert got.requested_by_task_id == "t1"
    assert got.iteration_ids == ()


def test_append_iteration_id_persists_tuple(workflow_store, task_center_run_id):
    req = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    after_first = workflow_store.append_iteration_id(req.id, "s1")
    after_second = workflow_store.append_iteration_id(req.id, "s2")
    assert after_first.iteration_ids == ("s1",)
    assert after_second.iteration_ids == ("s1", "s2")
    assert isinstance(after_second.iteration_ids, tuple)


def test_set_status_records_outcome_and_closed_at(
    workflow_store, task_center_run_id
):
    req = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    closed_at = datetime.now(UTC)
    updated = workflow_store.set_status(
        req.id,
        status=WorkflowStatus.SUCCEEDED,
        final_outcome={"outcome": "success"},
        closed_at=closed_at,
    )
    assert updated.status == WorkflowStatus.SUCCEEDED
    assert updated.final_outcome == {"outcome": "success"}
    assert updated.closed_at is not None


def test_list_for_parent_task_orders_by_created_at(
    workflow_store, task_center_run_id
):
    a = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task-A",
        goal="ga",
    )
    b = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task-A",
        goal="gb",
    )
    workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task-B",
        goal="gc",
    )
    listed = workflow_store.list_for_parent_task("parent-task-A")
    assert [r.id for r in listed] == [a.id, b.id]
