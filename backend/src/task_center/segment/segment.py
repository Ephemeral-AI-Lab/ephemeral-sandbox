"""TaskSegment domain DTO and enums."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum


class TaskSegmentStatus(StrEnum):
    OPEN = "open"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


class TaskSegmentCreationReason(StrEnum):
    INITIAL = "initial"
    PARTIAL_CONTINUATION = "partial_continuation"


@dataclass(frozen=True, slots=True)
class TaskSegment:
    """Immutable view of a persisted TaskSegment."""

    id: str
    complex_task_request_id: str
    sequence_no: int
    creation_reason: TaskSegmentCreationReason
    goal: str
    attempt_budget: int
    status: TaskSegmentStatus
    harness_graph_ids: tuple[str, ...]
    continuation_goal: str | None
    created_at: datetime
    updated_at: datetime
    closed_at: datetime | None

    @property
    def is_open(self) -> bool:
        return self.status == TaskSegmentStatus.OPEN

    @property
    def attempt_count(self) -> int:
        # A passing graph closes the segment immediately, so in practice this
        # equals the number of failed (or startup-failed) attempts. Do not
        # rely on that elsewhere.
        return len(self.harness_graph_ids)

    @property
    def has_budget_remaining(self) -> bool:
        return self.attempt_count < self.attempt_budget

    @property
    def latest_graph_id(self) -> str | None:
        return self.harness_graph_ids[-1] if self.harness_graph_ids else None
