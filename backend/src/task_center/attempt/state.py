"""Attempt domain DTO and enums."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum


class AttemptStage(StrEnum):
    PLANNING = "planning"
    GENERATING = "generating"
    EVALUATING = "evaluating"
    CLOSED = "closed"


class AttemptStatus(StrEnum):
    RUNNING = "running"
    PASSED = "passed"
    FAILED = "failed"


class AttemptFailReason(StrEnum):
    PLANNER_FAILED = "planner_failed"
    GENERATOR_FAILED = "generator_failed"
    EVALUATOR_FAILED = "evaluator_failed"
    STARTUP_FAILED = "startup_failed"


@dataclass(frozen=True, slots=True)
class Attempt:
    """Immutable view of a persisted Attempt."""

    id: str
    episode_id: str
    attempt_sequence_no: int
    stage: AttemptStage
    status: AttemptStatus
    planner_task_id: str | None
    task_specification: str | None
    evaluation_criteria: tuple[str, ...]
    generator_task_ids: tuple[str, ...]
    evaluator_task_id: str | None
    continuation_goal: str | None
    fail_reason: AttemptFailReason | None
    created_at: datetime
    updated_at: datetime
    closed_at: datetime | None
    context: str | None = None
    summary: str | None = None

    @property
    def is_closed(self) -> bool:
        return self.stage == AttemptStage.CLOSED

    @property
    def has_partial_continuation(self) -> bool:
        return self.continuation_goal is not None
