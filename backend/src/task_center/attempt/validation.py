"""Attempt-layer invariants. All raise ``TaskCenterInvariantViolation``."""

from __future__ import annotations

from typing import Any

from task_center.exceptions import TaskCenterInvariantViolation
from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.episode.episode import Episode
from task_center.task.models import TaskCenterTaskRole


def assert_attempt_sequence_contiguous(
    episode: Episode, new_sequence_no: int
) -> None:
    expected = len(episode.attempt_ids) + 1
    if new_sequence_no != expected:
        raise TaskCenterInvariantViolation(
            f"Attempt attempt_sequence_no must be contiguous: expected "
            f"{expected}, got {new_sequence_no}"
        )


def assert_fail_reason_present_on_failure(attempt: Attempt) -> None:
    if attempt.status == AttemptStatus.FAILED and attempt.fail_reason is None:
        raise TaskCenterInvariantViolation(
            f"Attempt {attempt.id!r} closed FAILED with no fail_reason"
        )


def assert_attempt_stage(
    attempt: Attempt, expected: AttemptStage
) -> None:
    if attempt.stage != expected:
        raise TaskCenterInvariantViolation(
            f"Attempt {attempt.id!r} expected stage {expected.value!r}, "
            f"got {attempt.stage.value!r}"
        )


def assert_attempt_not_closed(attempt: Attempt) -> None:
    if attempt.is_closed:
        raise TaskCenterInvariantViolation(
            f"Attempt {attempt.id!r} is already closed"
        )


def assert_valid_attempt_close(
    *,
    status: AttemptStatus,
    fail_reason: AttemptFailReason | None,
) -> None:
    if status == AttemptStatus.FAILED and fail_reason is None:
        raise TaskCenterInvariantViolation("Failed attempt close requires fail_reason")
    if status == AttemptStatus.PASSED and fail_reason is not None:
        raise TaskCenterInvariantViolation("Passed attempt close cannot have fail_reason")
    if status == AttemptStatus.RUNNING:
        raise TaskCenterInvariantViolation("Cannot close attempt with running status")


def assert_task_belongs_to_attempt(
    task: dict[str, Any], attempt: Attempt
) -> None:
    if task.get("task_center_attempt_id") != attempt.id:
        raise TaskCenterInvariantViolation(
            f"Task {task.get('id')!r} does not belong to Attempt "
            f"{attempt.id!r}"
        )


def assert_generator_task_for_submission(
    task: dict[str, Any], attempt: Attempt
) -> None:
    assert_task_belongs_to_attempt(task, attempt)
    if task.get("role") != TaskCenterTaskRole.GENERATOR.value:
        raise TaskCenterInvariantViolation(
            f"Task {task.get('id')!r} is not a generator task"
        )


def assert_evaluator_task_for_submission(
    task: dict[str, Any], attempt: Attempt
) -> None:
    assert_task_belongs_to_attempt(task, attempt)
    if task.get("role") != TaskCenterTaskRole.EVALUATOR.value:
        raise TaskCenterInvariantViolation(
            f"Task {task.get('id')!r} is not an evaluator task"
        )
