"""TaskCenter domain invariants — assertion helpers.

Each ``assert_*`` validates one harness lifecycle invariant and raises
:class:`TaskCenterInvariantViolation` on breach. Used by the goal lifecycle,
iteration attempt coordinator, attempt orchestrator, and stage advancer to fail fast on
illegal transitions instead of silently corrupting state.
"""

from __future__ import annotations

from typing import Any

from task_center._core.primitives import TaskCenterInvariantViolation
from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.iteration.state import Iteration, IterationStatus
from task_center.workflow.state import Workflow
from task_center._core.task_state import TaskCenterTaskRole


def assert_workflow_open(goal: Workflow) -> None:
    if not goal.is_open:
        raise TaskCenterInvariantViolation(f"Workflow {goal.id!r} is not open (status={goal.status})")


def assert_iteration_id_unique_in_workflow(goal: Workflow, iteration_id: str) -> None:
    if iteration_id in goal.iteration_ids:
        raise TaskCenterInvariantViolation(
            f"Iteration {iteration_id!r} already present in Workflow {goal.id!r} iteration list"
        )


def assert_iteration_sequence_contiguous(goal: Workflow, new_sequence_no: int) -> None:
    expected = len(goal.iteration_ids) + 1
    if new_sequence_no != expected:
        raise TaskCenterInvariantViolation(
            f"Iteration sequence_no must be contiguous: expected {expected}, got {new_sequence_no}"
        )


def assert_predecessor_has_deferred_goal_for_next_iteration(previous: Iteration) -> None:
    if previous.status != IterationStatus.SUCCEEDED:
        raise TaskCenterInvariantViolation(
            f"Continuation requires predecessor iteration {previous.id!r} to be SUCCEEDED, "
            f"not {previous.status}"
        )
    if previous.deferred_goal_for_next_iteration is None:
        raise TaskCenterInvariantViolation(
            f"Continuation requires predecessor iteration {previous.id!r} to have a "
            f"deferred_goal_for_next_iteration; none was recorded"
        )


def assert_iteration_open(iteration: Iteration) -> None:
    if not iteration.is_open:
        raise TaskCenterInvariantViolation(
            f"Iteration {iteration.id!r} is not open (status={iteration.status})"
        )


def assert_iteration_has_budget(iteration: Iteration) -> None:
    if not iteration.has_budget_remaining:
        raise TaskCenterInvariantViolation(
            f"Iteration {iteration.id!r} attempt budget exhausted "
            f"({iteration.attempt_count}/{iteration.attempt_budget})"
        )


def assert_attempt_belongs_to_iteration(attempt: Attempt, iteration: Iteration) -> None:
    if attempt.iteration_id != iteration.id:
        raise TaskCenterInvariantViolation(
            f"Attempt {attempt.id!r} (iteration {attempt.iteration_id!r}) does not "
            f"belong to Iteration {iteration.id!r}"
        )


def assert_attempt_sequence_contiguous(iteration: Iteration, new_sequence_no: int) -> None:
    expected = len(iteration.attempt_ids) + 1
    if new_sequence_no != expected:
        raise TaskCenterInvariantViolation(
            f"Attempt attempt_sequence_no must be contiguous: expected {expected}, "
            f"got {new_sequence_no}"
        )


def assert_fail_reason_present_on_failure(attempt: Attempt) -> None:
    if attempt.status == AttemptStatus.FAILED and attempt.fail_reason is None:
        raise TaskCenterInvariantViolation(
            f"Attempt {attempt.id!r} closed FAILED with no fail_reason"
        )


def assert_attempt_stage(attempt: Attempt, expected: AttemptStage) -> None:
    if attempt.stage != expected:
        raise TaskCenterInvariantViolation(
            f"Attempt {attempt.id!r} expected stage {expected.value!r}, got {attempt.stage.value!r}"
        )


def assert_attempt_not_closed(attempt: Attempt) -> None:
    if attempt.is_closed:
        raise TaskCenterInvariantViolation(f"Attempt {attempt.id!r} is already closed")


def assert_valid_attempt_close(
    *, status: AttemptStatus, fail_reason: AttemptFailReason | None
) -> None:
    if status == AttemptStatus.FAILED and fail_reason is None:
        raise TaskCenterInvariantViolation("Failed attempt close requires fail_reason")
    if status == AttemptStatus.PASSED and fail_reason is not None:
        raise TaskCenterInvariantViolation("Passed attempt close cannot have fail_reason")
    if status == AttemptStatus.RUNNING:
        raise TaskCenterInvariantViolation("Cannot close attempt with running status")


def assert_task_belongs_to_attempt(task: dict[str, Any], attempt: Attempt) -> None:
    if task.get("task_center_attempt_id") != attempt.id:
        raise TaskCenterInvariantViolation(
            f"Task {task.get('id')!r} does not belong to Attempt {attempt.id!r}"
        )


def assert_generator_task_for_submission(task: dict[str, Any], attempt: Attempt) -> None:
    assert_task_belongs_to_attempt(task, attempt)
    if task.get("role") != TaskCenterTaskRole.GENERATOR.value:
        raise TaskCenterInvariantViolation(f"Task {task.get('id')!r} is not a generator task")


def assert_evaluator_task_for_submission(task: dict[str, Any], attempt: Attempt) -> None:
    assert_task_belongs_to_attempt(task, attempt)
    if task.get("role") != TaskCenterTaskRole.EVALUATOR.value:
        raise TaskCenterInvariantViolation(f"Task {task.get('id')!r} is not an evaluator task")


__all__ = [
    "assert_attempt_belongs_to_iteration",
    "assert_attempt_not_closed",
    "assert_attempt_sequence_contiguous",
    "assert_attempt_stage",
    "assert_predecessor_has_deferred_goal_for_next_iteration",
    "assert_evaluator_task_for_submission",
    "assert_fail_reason_present_on_failure",
    "assert_generator_task_for_submission",
    "assert_workflow_open",
    "assert_iteration_has_budget",
    "assert_iteration_id_unique_in_workflow",
    "assert_iteration_open",
    "assert_iteration_sequence_contiguous",
    "assert_task_belongs_to_attempt",
    "assert_valid_attempt_close",
]
