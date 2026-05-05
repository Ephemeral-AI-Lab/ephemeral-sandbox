"""ComplexTaskRequest domain DTO and enums."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import Any, Literal


class ComplexTaskRequestStatus(StrEnum):
    OPEN = "open"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


@dataclass(frozen=True, slots=True)
class ComplexTaskRequest:
    """Immutable view of a persisted ComplexTaskRequest."""

    id: str
    task_center_run_id: str
    requested_by_task_id: str
    goal: str
    status: ComplexTaskRequestStatus
    task_segment_ids: tuple[str, ...]
    final_outcome: dict[str, Any] | None
    created_at: datetime
    updated_at: datetime
    closed_at: datetime | None

    @property
    def is_open(self) -> bool:
        return self.status == ComplexTaskRequestStatus.OPEN


@dataclass(frozen=True, slots=True)
class ComplexTaskCloseReport:
    """Final report attached to ``requested_by_task_id`` when the request closes.

    ``final_harness_graph_id`` is ``None`` for graph-less entry segments — the
    entry executor lives in a segment with zero ``HarnessGraph`` rows and
    closes via the entry-task controller rather than a passing graph.
    """

    complex_task_request_id: str
    requested_by_task_id: str
    outcome: Literal["success", "failed"]
    final_segment_id: str
    final_harness_graph_id: str | None

    def to_final_outcome(self) -> dict[str, str | None]:
        return {
            "outcome": self.outcome,
            "final_segment_id": self.final_segment_id,
            "final_harness_graph_id": self.final_harness_graph_id,
        }
