"""HarnessGraph domain DTO and enums."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum


class HarnessGraphStage(StrEnum):
    PLANNING = "planning"
    GENERATING = "generating"
    EVALUATING = "evaluating"
    CLOSED = "closed"


class HarnessGraphStatus(StrEnum):
    RUNNING = "running"
    PASSED = "passed"
    FAILED = "failed"


class HarnessGraphFailReason(StrEnum):
    PLANNER_FAILED = "planner_failed"
    GENERATOR_FAILED = "generator_failed"
    EVALUATOR_FAILED = "evaluator_failed"
    STARTUP_FAILED = "startup_failed"


@dataclass(frozen=True, slots=True)
class HarnessGraph:
    """Immutable view of a persisted HarnessGraph."""

    id: str
    task_segment_id: str
    graph_sequence_no: int
    stage: HarnessGraphStage
    status: HarnessGraphStatus
    planner_task_id: str | None
    task_specification: str | None
    evaluation_criteria: tuple[str, ...]
    generator_task_ids: tuple[str, ...]
    evaluator_task_id: str | None
    continuation_goal: str | None
    fail_reason: HarnessGraphFailReason | None
    created_at: datetime
    updated_at: datetime
    closed_at: datetime | None

    @property
    def is_closed(self) -> bool:
        return self.stage == HarnessGraphStage.CLOSED

    @property
    def has_partial_continuation(self) -> bool:
        return self.continuation_goal is not None
