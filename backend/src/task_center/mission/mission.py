"""Mission domain DTO and enums."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import Any, Literal


class MissionStatus(StrEnum):
    OPEN = "open"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


@dataclass(frozen=True, slots=True)
class Mission:
    """Immutable view of a persisted Mission."""

    id: str
    task_center_run_id: str
    requested_by_task_id: str
    goal: str
    status: MissionStatus
    episode_ids: tuple[str, ...]
    final_outcome: dict[str, Any] | None
    created_at: datetime
    updated_at: datetime
    closed_at: datetime | None
    context: str | None = None
    summary: str | None = None

    @property
    def is_open(self) -> bool:
        return self.status == MissionStatus.OPEN


@dataclass(frozen=True, slots=True)
class MissionCloseReport:
    """Final report attached to ``requested_by_task_id`` when the request closes.

    ``final_attempt_id`` is ``None`` for attempt-less entry episodes — the
    entry executor lives in a episode with zero ``Attempt`` rows and
    closes via the entry-task controller rather than a passing attempt.
    """

    mission_id: str
    requested_by_task_id: str
    outcome: Literal["success", "failed"]
    final_episode_id: str
    final_attempt_id: str | None

    def to_final_outcome(self) -> dict[str, str | None]:
        return {
            "outcome": self.outcome,
            "final_episode_id": self.final_episode_id,
            "final_attempt_id": self.final_attempt_id,
        }
