"""Workflow domain DTO and enums."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import Any, Literal


class WorkflowOriginKind(StrEnum):
    ENTRY = "entry"
    TASK = "task"


@dataclass(frozen=True, slots=True)
class WorkflowOrigin:
    """Where prompt text entered the goal lifecycle."""

    kind: WorkflowOriginKind
    task_center_run_id: str | None = None
    task_id: str | None = None

    @classmethod
    def entry(cls, *, task_center_run_id: str) -> "WorkflowOrigin":
        return cls(kind=WorkflowOriginKind.ENTRY, task_center_run_id=task_center_run_id)

    @classmethod
    def task(cls, *, task_id: str) -> "WorkflowOrigin":
        return cls(kind=WorkflowOriginKind.TASK, task_id=task_id)

    def __post_init__(self) -> None:
        if self.kind == WorkflowOriginKind.ENTRY:
            if not self.task_center_run_id or self.task_id is not None:
                raise ValueError("entry goal origin requires only task_center_run_id")
            return
        if self.kind == WorkflowOriginKind.TASK:
            if not self.task_id or self.task_center_run_id is not None:
                raise ValueError("task goal origin requires only task_id")
            return
        raise ValueError(f"Unsupported goal origin kind: {self.kind!r}")


class WorkflowStatus(StrEnum):
    OPEN = "open"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


@dataclass(frozen=True, slots=True)
class Workflow:
    """Immutable view of a persisted Workflow."""

    id: str
    task_center_run_id: str
    goal: str
    status: WorkflowStatus
    iteration_ids: tuple[str, ...]
    final_outcome: dict[str, Any] | None
    created_at: datetime
    updated_at: datetime
    closed_at: datetime | None
    origin_kind: WorkflowOriginKind = WorkflowOriginKind.TASK
    requested_by_task_id: str | None = None

    @property
    def is_open(self) -> bool:
        return self.status == WorkflowStatus.OPEN

    @property
    def origin(self) -> WorkflowOrigin:
        if self.origin_kind == WorkflowOriginKind.ENTRY:
            return WorkflowOrigin.entry(task_center_run_id=self.task_center_run_id)
        if self.requested_by_task_id is None:
            raise ValueError("task-origin goal is missing requested_by_task_id")
        return WorkflowOrigin.task(task_id=self.requested_by_task_id)


@dataclass(frozen=True, slots=True)
class WorkflowClosureReport:
    """Final report emitted when a goal closes.

    ``final_attempt_id`` is normally the passing or final failed attempt.
    It remains nullable for defensive compensation paths.
    """

    workflow_id: str
    task_center_run_id: str
    origin_kind: WorkflowOriginKind
    requested_by_task_id: str | None
    outcome: Literal["success", "failed"]
    final_iteration_id: str
    final_attempt_id: str | None

    def to_final_outcome(self) -> dict[str, str | None]:
        return {
            "outcome": self.outcome,
            "final_iteration_id": self.final_iteration_id,
            "final_attempt_id": self.final_attempt_id,
        }


WorkflowClosureDeliveryStatus = Literal["delivered", "already_delivered"]


@dataclass(frozen=True, slots=True)
class WorkflowClosureDeliveryResult:
    status: WorkflowClosureDeliveryStatus
    requested_by_task_id: str | None
    parent_attempt_id: str | None
