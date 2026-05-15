"""Episode domain DTO, enums, and closure-report DTOs."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import Any, Literal

from task_center.attempt.state import AttemptFailReason


class EpisodeStatus(StrEnum):
    OPEN = "open"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


class EpisodeCreationReason(StrEnum):
    INITIAL = "initial"
    PARTIAL_CONTINUATION = "partial_continuation"


@dataclass(frozen=True, slots=True)
class Episode:
    """Immutable view of a persisted Episode."""

    id: str
    mission_id: str
    sequence_no: int
    creation_reason: EpisodeCreationReason
    goal: str
    attempt_budget: int
    status: EpisodeStatus
    attempt_ids: tuple[str, ...]
    continuation_goal: str | None
    created_at: datetime
    updated_at: datetime
    closed_at: datetime | None
    # Denormalized from the episode's passing harness attempt at close. Both
    # null while open and on failed close.
    task_specification: str | None = None
    task_summary: str | None = None

    @property
    def is_open(self) -> bool:
        return self.status == EpisodeStatus.OPEN

    @property
    def attempt_count(self) -> int:
        # A passing attempt closes the episode immediately, so in practice this
        # equals the number of failed (or startup-failed) attempts. Do not
        # rely on that elsewhere.
        return len(self.attempt_ids)

    @property
    def has_budget_remaining(self) -> bool:
        return self.attempt_count < self.attempt_budget

    @property
    def latest_attempt_id(self) -> str | None:
        return self.attempt_ids[-1] if self.attempt_ids else None


@dataclass(frozen=True, slots=True)
class AttemptedPlanEntry:
    """One past attempt's structural state. Phase 06 fills the summary fields."""

    attempt_id: str
    attempt_sequence_no: int
    task_specification: str | None
    evaluation_criteria: tuple[str, ...]
    fail_reason: AttemptFailReason | None
    attempt_summary_id: str | None
    failure_landscape: dict[str, Any] | None


@dataclass(frozen=True, slots=True)
class TerminalSuccess:
    kind: Literal["terminal_success"] = "terminal_success"


@dataclass(frozen=True, slots=True)
class SuccessContinue:
    goal: str
    kind: Literal["success_continue"] = "success_continue"


@dataclass(frozen=True, slots=True)
class AttemptPlanFailed:
    failure_summary: str
    attempted_plan_history: tuple[AttemptedPlanEntry, ...]
    kind: Literal["attempt_plan_failed"] = "attempt_plan_failed"


ClosureOutcome = TerminalSuccess | SuccessContinue | AttemptPlanFailed


@dataclass(frozen=True, slots=True)
class EpisodeClosureReport:
    episode_id: str
    final_attempt_id: str
    outcome: ClosureOutcome
