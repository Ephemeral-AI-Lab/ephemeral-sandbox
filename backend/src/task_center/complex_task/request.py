"""ComplexTaskRequest domain DTO and enums."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import Any, Literal, cast


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

    Phase 04 wires the actual delivery to the requesting generator task.
    """

    complex_task_request_id: str
    requested_by_task_id: str
    outcome: Literal["success", "failed"]
    final_segment_id: str
    final_harness_graph_id: str

    def to_final_outcome(self) -> dict[str, str]:
        return {
            "outcome": self.outcome,
            "final_segment_id": self.final_segment_id,
            "final_harness_graph_id": self.final_harness_graph_id,
        }

    @classmethod
    def from_request(
        cls, request: ComplexTaskRequest
    ) -> "ComplexTaskCloseReport | None":
        if request.status not in (
            ComplexTaskRequestStatus.SUCCEEDED,
            ComplexTaskRequestStatus.FAILED,
        ):
            return None
        payload = request.final_outcome
        if not isinstance(payload, dict):
            raise ValueError(
                f"ComplexTaskRequest {request.id!r} is closed but has no "
                "final_outcome payload."
            )
        outcome = payload.get("outcome")
        final_segment_id = payload.get("final_segment_id")
        final_harness_graph_id = payload.get("final_harness_graph_id")
        if outcome not in ("success", "failed"):
            raise ValueError(
                f"ComplexTaskRequest {request.id!r} final_outcome.outcome is "
                f"{outcome!r}; expected 'success' or 'failed'."
            )
        if not isinstance(final_segment_id, str) or not final_segment_id:
            raise ValueError(
                f"ComplexTaskRequest {request.id!r} "
                "final_outcome.final_segment_id is missing."
            )
        if not isinstance(final_harness_graph_id, str):
            raise ValueError(
                f"ComplexTaskRequest {request.id!r} final_outcome."
                "final_harness_graph_id is missing."
            )
        return cls(
            complex_task_request_id=request.id,
            requested_by_task_id=request.requested_by_task_id,
            outcome=cast(Literal["success", "failed"], outcome),
            final_segment_id=final_segment_id,
            final_harness_graph_id=final_harness_graph_id,
        )
