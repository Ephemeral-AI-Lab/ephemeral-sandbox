"""US-009: IterationStore.close_succeeded atomicity + denormalization."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.iteration.state import (
    IterationCreationReason,
    IterationStatus,
)


def _seed_segment(goal_store, iteration_store, task_center_run_id):
    req = goal_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task",
        goal="g",
    )
    return iteration_store.insert(
        goal_id=req.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )


def test_close_succeeded_writes_status_spec_summary_atomically(
    goal_store, iteration_store, task_center_run_id
):
    seg = _seed_segment(goal_store, iteration_store, task_center_run_id)
    closed = iteration_store.close_succeeded(
        seg.id,
        plan_spec="resulting spec",
        task_summary="evaluator pass summary",
        closed_at=datetime.now(UTC),
    )
    assert closed.status == IterationStatus.SUCCEEDED
    assert closed.plan_spec == "resulting spec"
    assert closed.task_summary == "evaluator pass summary"
    assert closed.closed_at is not None


def test_close_succeeded_persists_through_get(
    goal_store, iteration_store, task_center_run_id
):
    seg = _seed_segment(goal_store, iteration_store, task_center_run_id)
    iteration_store.close_succeeded(
        seg.id,
        plan_spec="spec",
        task_summary="summary",
    )
    reloaded = iteration_store.get(seg.id)
    assert reloaded is not None
    assert reloaded.plan_spec == "spec"
    assert reloaded.task_summary == "summary"


def test_failed_close_leaves_denormalized_fields_null(
    goal_store, iteration_store, task_center_run_id
):
    seg = _seed_segment(goal_store, iteration_store, task_center_run_id)
    failed = iteration_store.set_status(
        seg.id,
        status=IterationStatus.FAILED,
        closed_at=datetime.now(UTC),
    )
    assert failed.status == IterationStatus.FAILED
    assert failed.plan_spec is None
    assert failed.task_summary is None


def test_close_succeeded_unknown_segment_raises(iteration_store):
    with pytest.raises(LookupError):
        iteration_store.close_succeeded(
            "no-such-iteration",
            plan_spec="x",
            task_summary="y",
        )


def test_initial_iteration_has_null_denormalized_fields(
    goal_store, iteration_store, task_center_run_id
):
    seg = _seed_segment(goal_store, iteration_store, task_center_run_id)
    assert seg.plan_spec is None
    assert seg.task_summary is None
