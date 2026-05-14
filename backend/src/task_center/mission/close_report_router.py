"""MissionClosureReport delivery router.

Owns the single delivery path from ``MissionHandler.close_mission`` to the
parent ``AttemptOrchestrator.apply_mission_closure_report``. The runtime
assumes no process restart: while a parent generator task is in
``WAITING_MISSION`` its attempt cannot reach quiescence and its
orchestrator stays registered. A missing orchestrator at delivery time
is a hard ``TaskCenterInvariantViolation``.
"""

from __future__ import annotations

from task_center.attempt.runtime import AttemptDeps
from task_center._core.types import TaskCenterInvariantViolation
from task_center.mission.state import (
    CloseReportDeliveryResult,
    MissionClosureReport,
)
from task_center.task_state import TaskCenterTaskStatus


class MissionClosureReportRouter:
    """Single delivery path for final ``MissionClosureReport``s."""

    def __init__(self, *, runtime: AttemptDeps) -> None:
        self._runtime = runtime

    def deliver(
        self, report: MissionClosureReport
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

        target = self._runtime.lifecycle_target_for(
            task_id=report.requested_by_task_id, attempt_id=attempt_id
        )
        if target is None:
            kind = (
                "entry controller"
                if attempt_id is None
                else f"AttemptOrchestrator for attempt {attempt_id!r}"
            )
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r}: "
                f"{kind} is not registered; close-report delivery cannot "
                "proceed."
            )
        target.apply_mission_closure_report(report)
        return CloseReportDeliveryResult(
            status="delivered",
            requested_by_task_id=report.requested_by_task_id,
            parent_attempt_id=attempt_id,
        )
