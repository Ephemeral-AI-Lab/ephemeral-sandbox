"""Generator DAG helper tests."""

from __future__ import annotations

import pytest

from task_center._core.primitives import TaskCenterInvariantViolation
from task_center.attempt.generator_dag import (
    ordered_generator_tasks,
    ready_pending_generator_ids,
    summarize_generator_dag,
)
from task_center.submissions import PlannedGeneratorTask


def _task(task_id: str, status: str, needs: tuple[str, ...] = ()) -> dict:
    return {
        "id": task_id,
        "status": status,
        "needs": list(needs),
    }


def test_ordered_generator_tasks_topological_and_stable():
    a = PlannedGeneratorTask("a", "executor", (), "A")
    b = PlannedGeneratorTask("b", "executor", ("a",), "B")
    c = PlannedGeneratorTask("c", "verifier", ("a",), "C")

    assert ordered_generator_tasks((b, c, a)) == (a, b, c)


def test_ordered_generator_tasks_rejects_dangling_dep():
    task = PlannedGeneratorTask("a", "executor", ("missing",), "A")

    with pytest.raises(TaskCenterInvariantViolation):
        ordered_generator_tasks((task,))


def test_ordered_generator_tasks_rejects_cycle():
    a = PlannedGeneratorTask("a", "executor", ("b",), "A")
    b = PlannedGeneratorTask("b", "executor", ("a",), "B")

    with pytest.raises(TaskCenterInvariantViolation):
        ordered_generator_tasks((a, b))


def test_ready_pending_generator_ids_requires_done_deps():
    records = [
        _task("a", "done"),
        _task("b", "pending", ("a",)),
        _task("c", "pending", ("b",)),
    ]

    assert ready_pending_generator_ids(records) == ("b",)


def test_pending_dependents_of_failed_task_are_quiescent_not_started():
    records = [
        _task("a", "failed"),
        _task("b", "pending", ("a",)),
        _task("c", "pending", ("b",)),
        _task("d", "running"),
    ]

    state = summarize_generator_dag(records)
    assert not state.all_quiescent


def test_pending_dependents_of_failed_task_close_after_siblings_finish():
    records = [
        _task("a", "failed"),
        _task("b", "pending", ("a",)),
        _task("c", "pending", ("b",)),
        _task("d", "done"),
    ]

    state = summarize_generator_dag(records)
    assert state.all_quiescent
    assert not state.all_done
    assert state.any_failed_or_blocked


def test_waiting_workflow_is_not_quiescent_or_done():
    records = [_task("a", "waiting_workflow")]

    state = summarize_generator_dag(records)
    assert not state.all_quiescent
    assert not state.all_done
