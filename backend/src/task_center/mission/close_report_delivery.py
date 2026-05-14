"""MissionCloseReport delivery router.

Owns the single delivery path from ``MissionHandler.close_mission``
to the parent ``AttemptOrchestrator.apply_mission_close_report``.

The runtime assumes no process restart: while a parent generator task is in
``WAITING_MISSION`` its attempt cannot reach quiescence and its
orchestrator stays registered. Therefore close-report delivery is always
synchronous against an active parent orchestrator; a missing orchestrator at
delivery time is a hard ``TaskCenterInvariantViolation``.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from task_center.mission.mission import MissionCloseReport
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.attempt.runtime import AttemptDeps
from task_center.task.models import TaskCenterTaskStatus

CloseReportDeliveryStatus = Literal[
    "delivered",
    "already_delivered",
]


@dataclass(frozen=True, slots=True)
class CloseReportDeliveryResult:
    status: CloseReportDeliveryStatus
    requested_by_task_id: str
    parent_attempt_id: str | None


class MissionCloseReportRouter:
    """Single delivery path for final ``MissionCloseReport``s."""

    def __init__(self, *, runtime: AttemptDeps) -> None:
        self._runtime = runtime

    def deliver(
        self, report: MissionCloseReport
    ) -> CloseReportDeliveryResult:
        task = self._runtime.task_store.get_task(report.requested_by_task_id)
        if task is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r} was not found."
            )
        attempt_id = str(task.get("task_center_attempt_id") or "") or None
        status = str(task.get("status") or "")
        if status in (
            TaskCenterTaskStatus.DONE.value,
            TaskCenterTaskStatus.FAILED.value,
        ):
            return CloseReportDeliveryResult(
                status="already_delivered",
                requested_by_task_id=report.requested_by_task_id,
                parent_attempt_id=attempt_id,
            )
        if status != TaskCenterTaskStatus.WAITING_MISSION.value:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r} is not waiting "
                "on a mission."
            )

        if attempt_id is None:
            # Entry mode: parent is the top-level entry executor. Route
            # through the runtime's EntryTaskController instead of the
            # orchestrator registry.
            controller = self._runtime.entry_task_controller_for(
                report.requested_by_task_id
            )
            if controller is None:
                raise TaskCenterInvariantViolation(
                    f"TaskCenter task {report.requested_by_task_id!r} is "
                    "entry-mode but no entry controller is bound to it; "
                    "close-report delivery cannot proceed."
                )
            controller.apply_mission_close_report(report)
            return CloseReportDeliveryResult(
                status="delivered",
                requested_by_task_id=report.requested_by_task_id,
                parent_attempt_id=None,
            )

        orchestrator = self._runtime.orchestrator_registry.get(attempt_id)
        if orchestrator is None:
            raise TaskCenterInvariantViolation(
                f"Parent AttemptOrchestrator for attempt {attempt_id!r} is "
                "not registered; close-report delivery requires an active "
                "parent orchestrator."
            )
        orchestrator.apply_mission_close_report(report)
        return CloseReportDeliveryResult(
            status="delivered",
            requested_by_task_id=report.requested_by_task_id,
            parent_attempt_id=attempt_id,
        )
