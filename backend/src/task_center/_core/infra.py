"""TaskCenter cross-cutting infra — audit emitter + domain invariants.

Phase 7b bundle: collapses former `_core/audit.py` and `_core/invariants.py`
into a single infrastructure module.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from enum import StrEnum
from typing import Any

from audit.base import AuditEvent, AuditNode, AuditSink, NoopAuditSink

from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.episode.state import Episode, EpisodeStatus
from task_center._core.types import TaskCenterInvariantViolation
from task_center.mission.state import Mission
from task_center.task_state import TaskCenterTaskRole


# ---- Audit event types + emitter -------------------------------------------


class TaskCenterAuditEventType(StrEnum):
    """Every audit event type the TaskCenter package emits."""

    TASK_READY = "task_center.task.ready"
    TASK_LAUNCHED = "task_center.task.launched"
    TASK_FAILED = "task_center.task.failed"


TASK_READY: str = TaskCenterAuditEventType.TASK_READY.value
TASK_LAUNCHED: str = TaskCenterAuditEventType.TASK_LAUNCHED.value
TASK_FAILED: str = TaskCenterAuditEventType.TASK_FAILED.value


class TaskCenterAuditEmitter:
    """Small write-only facade around a shared audit sink."""

    def __init__(self, sink: AuditSink | None = None) -> None:
        self._sink = sink if sink is not None else NoopAuditSink()

    def publish(
        self,
        event_type: str,
        *,
        node: AuditNode,
        payload: Mapping[str, Any] | None = None,
    ) -> None:
        self._sink.publish(
            AuditEvent(
                source="task_center",
                type=event_type,
                node=node,
                payload=dict(payload or {}),
            )
        )

    def task_ready(
        self,
        task: Mapping[str, Any],
        *,
        trial_id: str | None,
        satisfied_dependency_ids: Sequence[str],
    ) -> None:
        self.publish(
            TASK_READY,
            node=_task_node(task, trial_id=trial_id),
            payload={
                **_task_payload(task),
                "status_from": "pending",
                "status_to": "pending",
                "satisfied_dependency_ids": [str(dep) for dep in satisfied_dependency_ids],
            },
        )

    def task_launched(
        self,
        task: Mapping[str, Any],
        *,
        trial_id: str | None,
        status_from: str = "pending",
    ) -> None:
        self.publish(
            TASK_LAUNCHED,
            node=_task_node(task, trial_id=trial_id),
            payload={
                **_task_payload(task),
                "status_from": status_from,
                "status_to": str(task.get("status") or "running"),
            },
        )

    def task_failed(
        self,
        task: Mapping[str, Any],
        *,
        trial_id: str | None,
        status_from: str = "running",
        fail_reason: str = "",
        summary: str = "",
    ) -> None:
        self.publish(
            TASK_FAILED,
            node=_task_node(task, trial_id=trial_id),
            payload={
                **_task_payload(task),
                "status_from": status_from,
                "status_to": str(task.get("status") or "failed"),
                "fail_reason": fail_reason or None,
                "summary": summary or None,
            },
        )


def _task_node(task: Mapping[str, Any], *, trial_id: str | None) -> AuditNode:
    return AuditNode(
        task_center_run_id=_text(task.get("task_center_run_id")),
        attempt_id=_text(trial_id or task.get("task_center_attempt_id")),
        task_center_task_id=_text(task.get("id")),
        agent_name=_text(task.get("agent_name")),
    )


def _task_payload(task: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "run_id": _text(task.get("task_center_run_id")),
        "attempt_id": _text(task.get("task_center_attempt_id")),
        "task_center_task_id": _text(task.get("id")),
        "role": _text(task.get("role")),
        "agent_name": _text(task.get("agent_name")),
        "needs": [str(dep) for dep in task.get("needs", ()) or ()],
        "context_packet_id": _text(task.get("context_packet_id")),
    }


def _text(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


# ---- Domain invariants -----------------------------------------------------


def assert_goal_open(goal: Mission) -> None:
    if not goal.is_open:
        raise TaskCenterInvariantViolation(
            f"Goal {goal.id!r} is not open (status={goal.status})"
        )


def assert_iteration_id_unique_in_goal(goal: Mission, iteration_id: str) -> None:
    if iteration_id in goal.episode_ids:
        raise TaskCenterInvariantViolation(
            f"Iteration {iteration_id!r} already present in Goal {goal.id!r} iteration list"
        )


def assert_iteration_sequence_contiguous(goal: Mission, new_sequence_no: int) -> None:
    expected = len(goal.episode_ids) + 1
    if new_sequence_no != expected:
        raise TaskCenterInvariantViolation(
            f"Iteration sequence_no must be contiguous: expected {expected}, got {new_sequence_no}"
        )


def assert_continuation_iteration_predecessor(previous: Episode) -> None:
    if previous.status != EpisodeStatus.SUCCEEDED:
        raise TaskCenterInvariantViolation(
            f"Continuation requires predecessor iteration {previous.id!r} to be SUCCEEDED, "
            f"not {previous.status}"
        )
    if previous.continuation_goal is None:
        raise TaskCenterInvariantViolation(
            f"Continuation requires predecessor iteration {previous.id!r} to have a "
            f"continuation_goal; none was recorded"
        )


def assert_iteration_open(iteration: Episode) -> None:
    if not iteration.is_open:
        raise TaskCenterInvariantViolation(
            f"Iteration {iteration.id!r} is not open (status={iteration.status})"
        )


def assert_iteration_has_budget(iteration: Episode) -> None:
    if not iteration.has_budget_remaining:
        raise TaskCenterInvariantViolation(
            f"Iteration {iteration.id!r} trial budget exhausted "
            f"({iteration.attempt_count}/{iteration.attempt_budget})"
        )


def assert_trial_belongs_to_iteration(trial: Attempt, iteration: Episode) -> None:
    if trial.episode_id != iteration.id:
        raise TaskCenterInvariantViolation(
            f"Trial {trial.id!r} (iteration {trial.episode_id!r}) does not "
            f"belong to Iteration {iteration.id!r}"
        )


def assert_trial_sequence_contiguous(iteration: Episode, new_sequence_no: int) -> None:
    expected = len(iteration.attempt_ids) + 1
    if new_sequence_no != expected:
        raise TaskCenterInvariantViolation(
            f"Trial trial_sequence_no must be contiguous: expected {expected}, "
            f"got {new_sequence_no}"
        )


def assert_fail_reason_present_on_failure(trial: Attempt) -> None:
    if trial.status == AttemptStatus.FAILED and trial.fail_reason is None:
        raise TaskCenterInvariantViolation(
            f"Trial {trial.id!r} closed FAILED with no fail_reason"
        )


def assert_trial_stage(trial: Attempt, expected: AttemptStage) -> None:
    if trial.stage != expected:
        raise TaskCenterInvariantViolation(
            f"Trial {trial.id!r} expected stage {expected.value!r}, "
            f"got {trial.stage.value!r}"
        )


def assert_trial_not_closed(trial: Attempt) -> None:
    if trial.is_closed:
        raise TaskCenterInvariantViolation(f"Trial {trial.id!r} is already closed")


def assert_valid_trial_close(
    *, status: AttemptStatus, fail_reason: AttemptFailReason | None
) -> None:
    if status == AttemptStatus.FAILED and fail_reason is None:
        raise TaskCenterInvariantViolation("Failed trial close requires fail_reason")
    if status == AttemptStatus.PASSED and fail_reason is not None:
        raise TaskCenterInvariantViolation("Passed trial close cannot have fail_reason")
    if status == AttemptStatus.RUNNING:
        raise TaskCenterInvariantViolation("Cannot close trial with running status")


def assert_task_belongs_to_trial(task: dict[str, Any], trial: Attempt) -> None:
    if task.get("task_center_attempt_id") != trial.id:
        raise TaskCenterInvariantViolation(
            f"Task {task.get('id')!r} does not belong to Trial {trial.id!r}"
        )


def assert_generator_task_for_submission(task: dict[str, Any], trial: Attempt) -> None:
    assert_task_belongs_to_trial(task, trial)
    if task.get("role") != TaskCenterTaskRole.GENERATOR.value:
        raise TaskCenterInvariantViolation(f"Task {task.get('id')!r} is not a generator task")


def assert_evaluator_task_for_submission(task: dict[str, Any], trial: Attempt) -> None:
    assert_task_belongs_to_trial(task, trial)
    if task.get("role") != TaskCenterTaskRole.EVALUATOR.value:
        raise TaskCenterInvariantViolation(f"Task {task.get('id')!r} is not an evaluator task")


__all__ = [
    "TASK_FAILED",
    "TASK_LAUNCHED",
    "TASK_READY",
    "TaskCenterAuditEmitter",
    "TaskCenterAuditEventType",
    "assert_continuation_iteration_predecessor",
    "assert_evaluator_task_for_submission",
    "assert_fail_reason_present_on_failure",
    "assert_generator_task_for_submission",
    "assert_goal_open",
    "assert_iteration_has_budget",
    "assert_iteration_id_unique_in_goal",
    "assert_iteration_open",
    "assert_iteration_sequence_contiguous",
    "assert_task_belongs_to_trial",
    "assert_trial_belongs_to_iteration",
    "assert_trial_not_closed",
    "assert_trial_sequence_contiguous",
    "assert_trial_stage",
    "assert_valid_trial_close",
]
