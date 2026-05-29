"""Invariant tests across goal, iteration, and attempt levels."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center._core.invariants import (
    assert_attempt_belongs_to_iteration,
    assert_attempt_sequence_contiguous,
    assert_predecessor_has_deferred_goal_for_next_iteration,
    assert_fail_reason_present_on_failure,
    assert_workflow_open,
    assert_iteration_has_budget,
    assert_iteration_id_unique_in_workflow,
    assert_iteration_open,
    assert_iteration_sequence_contiguous,
)
from task_center.iteration import OpenIterationCoordinatorRegistry
from task_center.workflow.state import (
    Workflow,
    WorkflowStatus,
)
from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.iteration.state import (
    Iteration,
    IterationCreationReason,
    IterationStatus,
)
from task_center._core.primitives import TaskCenterInvariantViolation


def _goal(
    status: WorkflowStatus = WorkflowStatus.OPEN,
    iteration_ids: tuple[str, ...] = (),
) -> Workflow:
    now = datetime.now(UTC)
    return Workflow(
        id="r1",
        task_center_run_id="run1",
        requested_by_task_id="t1",
        goal="g",
        status=status,
        iteration_ids=iteration_ids,
        final_outcome=None,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


def _iteration(
    *,
    status: IterationStatus = IterationStatus.OPEN,
    attempt_ids: tuple[str, ...] = (),
    deferred_goal_for_next_iteration: str | None = None,
    attempt_budget: int = 2,
    iteration_id: str = "s1",
) -> Iteration:
    now = datetime.now(UTC)
    return Iteration(
        id=iteration_id,
        workflow_id="r1",
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=attempt_budget,
        status=status,
        attempt_ids=attempt_ids,
        deferred_goal_for_next_iteration=deferred_goal_for_next_iteration,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


def _attempt(
    *,
    status: AttemptStatus = AttemptStatus.RUNNING,
    fail_reason: AttemptFailReason | None = None,
    iteration_id: str = "s1",
    attempt_id: str = "g1",
) -> Attempt:
    now = datetime.now(UTC)
    return Attempt(
        id=attempt_id,
        iteration_id=iteration_id,
        attempt_sequence_no=1,
        stage=AttemptStage.PLAN,
        status=status,
        planner_task_id=None,
        plan_spec=None,
        evaluation_criteria=(),
        generator_task_ids=(),
        evaluator_task_id=None,
        deferred_goal_for_next_iteration=None,
        fail_reason=fail_reason,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


# ---- Workflow-level ---------------------------------------------------------


def test_assert_goal_open_passes_for_open():
    assert_workflow_open(_goal(status=WorkflowStatus.OPEN))


def test_assert_goal_open_fails_for_closed():
    for status in (
        WorkflowStatus.SUCCEEDED,
        WorkflowStatus.FAILED,
        WorkflowStatus.CANCELLED,
    ):
        with pytest.raises(TaskCenterInvariantViolation):
            assert_workflow_open(_goal(status=status))


def test_assert_iteration_id_unique_in_goal():
    assert_iteration_id_unique_in_workflow(
        _goal(iteration_ids=("s1", "s2")), "s3"
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_iteration_id_unique_in_workflow(
            _goal(iteration_ids=("s1",)), "s1"
        )


def test_assert_iteration_sequence_contiguous():
    assert_iteration_sequence_contiguous(_goal(iteration_ids=()), 1)
    assert_iteration_sequence_contiguous(_goal(iteration_ids=("s1",)), 2)
    with pytest.raises(TaskCenterInvariantViolation):
        assert_iteration_sequence_contiguous(_goal(iteration_ids=("s1",)), 1)
    with pytest.raises(TaskCenterInvariantViolation):
        assert_iteration_sequence_contiguous(_goal(iteration_ids=("s1",)), 3)


def test_assert_continuation_iteration_predecessor_requires_succeeded_with_goal():
    succeeded_with_goal = _iteration(
        status=IterationStatus.SUCCEEDED, deferred_goal_for_next_iteration="next"
    )
    assert_predecessor_has_deferred_goal_for_next_iteration(succeeded_with_goal)

    with pytest.raises(TaskCenterInvariantViolation):
        assert_predecessor_has_deferred_goal_for_next_iteration(
            _iteration(status=IterationStatus.OPEN, deferred_goal_for_next_iteration="next")
        )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_predecessor_has_deferred_goal_for_next_iteration(
            _iteration(status=IterationStatus.SUCCEEDED, deferred_goal_for_next_iteration=None)
        )


# ---- Iteration-level ----------------------------------------------------


def test_assert_iteration_open():
    assert_iteration_open(_iteration(status=IterationStatus.OPEN))
    with pytest.raises(TaskCenterInvariantViolation):
        assert_iteration_open(_iteration(status=IterationStatus.SUCCEEDED))


def test_assert_iteration_has_budget():
    assert_iteration_has_budget(_iteration(attempt_budget=2, attempt_ids=()))
    assert_iteration_has_budget(
        _iteration(attempt_budget=2, attempt_ids=("g1",))
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_iteration_has_budget(
            _iteration(attempt_budget=2, attempt_ids=("g1", "g2"))
        )


def test_assert_attempt_belongs_to_iteration():
    assert_attempt_belongs_to_iteration(
        _attempt(iteration_id="s1"), _iteration(iteration_id="s1")
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_attempt_belongs_to_iteration(
            _attempt(iteration_id="s1"), _iteration(iteration_id="s2")
        )


# ---- Attempt-level ------------------------------------------------------


def test_assert_attempt_sequence_contiguous():
    assert_attempt_sequence_contiguous(_iteration(attempt_ids=()), 1)
    assert_attempt_sequence_contiguous(_iteration(attempt_ids=("g1",)), 2)
    with pytest.raises(TaskCenterInvariantViolation):
        assert_attempt_sequence_contiguous(_iteration(attempt_ids=("g1",)), 1)


def test_assert_fail_reason_present_on_failure():
    assert_fail_reason_present_on_failure(
        _attempt(status=AttemptStatus.PASSED)
    )
    assert_fail_reason_present_on_failure(
        _attempt(
            status=AttemptStatus.FAILED,
            fail_reason=AttemptFailReason.GENERATOR_FAILED,
        )
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_fail_reason_present_on_failure(
            _attempt(status=AttemptStatus.FAILED, fail_reason=None)
        )


# ---- Coordinator registry -----------------------------------------------


def test_open_iteration_coordinators_enforces_uniqueness():
    reg = OpenIterationCoordinatorRegistry()

    class _Fake:
        iteration_id = "s1"

    reg.register(_Fake())  # type: ignore[arg-type]
    assert reg.get("s1") is not None
    with pytest.raises(TaskCenterInvariantViolation):
        reg.register(_Fake())  # type: ignore[arg-type]
    reg.deregister("s1")
    assert reg.get("s1") is None
