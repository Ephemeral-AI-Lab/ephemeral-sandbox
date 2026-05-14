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
from task_center.exceptions import TaskCenterInvariantViolation
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
        attempt_id: str | None,
        satisfied_dependency_ids: Sequence[str],
    ) -> None:
        self.publish(
            TASK_READY,
            node=_task_node(task, attempt_id=attempt_id),
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
        attempt_id: str | None,
        status_from: str = "pending",
    ) -> None:
        self.publish(
            TASK_LAUNCHED,
            node=_task_node(task, attempt_id=attempt_id),
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
        attempt_id: str | None,
        status_from: str = "running",
        fail_reason: str = "",
        summary: str = "",
    ) -> None:
        self.publish(
            TASK_FAILED,
            node=_task_node(task, attempt_id=attempt_id),
            payload={
                **_task_payload(task),
                "status_from": status_from,
                "status_to": str(task.get("status") or "failed"),
                "fail_reason": fail_reason or None,
                "summary": summary or None,
            },
        )


def _task_node(task: Mapping[str, Any], *, attempt_id: str | None) -> AuditNode:
    return AuditNode(
        task_center_run_id=_text(task.get("task_center_run_id")),
        attempt_id=_text(attempt_id or task.get("task_center_attempt_id")),
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


def assert_mission_open(mission: Mission) -> None:
    if not mission.is_open:
        raise TaskCenterInvariantViolation(
            f"Mission {mission.id!r} is not open (status={mission.status})"
        )


def assert_episode_id_unique_in_mission(mission: Mission, episode_id: str) -> None:
    if episode_id in mission.episode_ids:
        raise TaskCenterInvariantViolation(
            f"Episode {episode_id!r} already present in Mission {mission.id!r} episode list"
        )


def assert_episode_sequence_contiguous(mission: Mission, new_sequence_no: int) -> None:
    expected = len(mission.episode_ids) + 1
    if new_sequence_no != expected:
        raise TaskCenterInvariantViolation(
            f"Episode sequence_no must be contiguous: expected {expected}, got {new_sequence_no}"
        )


def assert_continuation_episode_predecessor(previous: Episode) -> None:
    if previous.status != EpisodeStatus.SUCCEEDED:
        raise TaskCenterInvariantViolation(
            f"Continuation requires predecessor episode {previous.id!r} to be SUCCEEDED, "
            f"not {previous.status}"
        )
    if previous.continuation_goal is None:
        raise TaskCenterInvariantViolation(
            f"Continuation requires predecessor episode {previous.id!r} to have a "
            f"continuation_goal; none was recorded"
        )


def assert_episode_open(episode: Episode) -> None:
    if not episode.is_open:
        raise TaskCenterInvariantViolation(
            f"Episode {episode.id!r} is not open (status={episode.status})"
        )


def assert_episode_has_budget(episode: Episode) -> None:
    if not episode.has_budget_remaining:
        raise TaskCenterInvariantViolation(
            f"Episode {episode.id!r} attempt budget exhausted "
            f"({episode.attempt_count}/{episode.attempt_budget})"
        )


def assert_attempt_belongs_to_episode(attempt: Attempt, episode: Episode) -> None:
    if attempt.episode_id != episode.id:
        raise TaskCenterInvariantViolation(
            f"Attempt {attempt.id!r} (episode {attempt.episode_id!r}) does not "
            f"belong to Episode {episode.id!r}"
        )


def assert_attempt_sequence_contiguous(episode: Episode, new_sequence_no: int) -> None:
    expected = len(episode.attempt_ids) + 1
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
            f"Attempt {attempt.id!r} expected stage {expected.value!r}, "
            f"got {attempt.stage.value!r}"
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
    "TASK_FAILED",
    "TASK_LAUNCHED",
    "TASK_READY",
    "TaskCenterAuditEmitter",
    "TaskCenterAuditEventType",
    "assert_attempt_belongs_to_episode",
    "assert_attempt_not_closed",
    "assert_attempt_sequence_contiguous",
    "assert_attempt_stage",
    "assert_continuation_episode_predecessor",
    "assert_episode_has_budget",
    "assert_episode_id_unique_in_mission",
    "assert_episode_open",
    "assert_episode_sequence_contiguous",
    "assert_evaluator_task_for_submission",
    "assert_fail_reason_present_on_failure",
    "assert_generator_task_for_submission",
    "assert_mission_open",
    "assert_task_belongs_to_attempt",
    "assert_valid_attempt_close",
]
