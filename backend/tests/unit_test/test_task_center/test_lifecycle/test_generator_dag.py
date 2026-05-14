"""Generator DAG helper tests."""

from __future__ import annotations

import pytest

from task_center._core.types import TaskCenterInvariantViolation
from task_center.attempt.generator_dag import (
    all_generators_done,
    all_generators_quiescent,
    blocked_descendant_ids,
    ordered_generator_tasks,
    ready_pending_generator_ids,
)
from task_center.task_state import PlannedGeneratorTask


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


def test_blocked_descendant_ids_blocks_pending_descendants():
    records = [
        _task("a", "failed"),
        _task("b", "pending", ("a",)),
        _task("c", "pending", ("b",)),
        _task("d", "running"),
    ]

    assert blocked_descendant_ids(
        failed_task_id="a", task_records=records
    ) == ("b", "c")


def test_running_dependent_of_failed_task_is_invariant_violation():
    records = [
        _task("a", "failed"),
        _task("b", "running", ("a",)),
    ]

    with pytest.raises(TaskCenterInvariantViolation):
        blocked_descendant_ids(failed_task_id="a", task_records=records)


def test_waiting_mission_is_not_quiescent_or_done():
    records = [_task("a", "waiting_mission")]

    assert not all_generators_quiescent(records)
    assert not all_generators_done(records)
