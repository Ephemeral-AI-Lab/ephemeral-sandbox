"""WorkflowClosureReport delivery router.

Owns the delivery path from ``WorkflowLifecycle.close_workflow`` to either the run
itself (entry-origin goals) or the parent
``AttemptOrchestrator.apply_workflow_closure_report`` (task-origin goals). The
runtime assumes no process restart: while a parent generator task is in
``WAITING_WORKFLOW`` its attempt cannot reach quiescence and its
orchestrator stays registered. A missing orchestrator at delivery time
is a hard ``TaskCenterInvariantViolation``.
"""

from __future__ import annotations

from task_center.attempt.deps import AttemptDeps
from task_center._core.primitives import TaskCenterInvariantViolation
from task_center.workflow.state import (
    WorkflowClosureDeliveryResult,
    WorkflowClosureReport,
    WorkflowOriginKind,
)
from task_center._core.task_state import TaskCenterTaskStatus


class WorkflowClosureReportRouter:
    """Single delivery path for final ``WorkflowClosureReport``s."""

    def __init__(self, *, runtime: AttemptDeps) -> None:
        self._runtime = runtime

    def deliver(self, report: WorkflowClosureReport) -> WorkflowClosureDeliveryResult:
        if report.origin_kind == WorkflowOriginKind.ENTRY:
            return self._deliver_entry_origin(report)
        if report.requested_by_task_id is None:
            raise TaskCenterInvariantViolation(
                f"Task-origin goal {report.workflow_id!r} has no requested_by_task_id."
            )
        task = self._runtime.task_store.get_task(report.requested_by_task_id)
        if task is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r} was not found."
            )
        attempt_id = task.get("task_center_attempt_id") or None
        status = task.get("status") or ""
        if status in (
            TaskCenterTaskStatus.DONE.value,
            TaskCenterTaskStatus.FAILED.value,
        ):
            return WorkflowClosureDeliveryResult(
                status="already_delivered",
                requested_by_task_id=report.requested_by_task_id,
                parent_attempt_id=attempt_id,
            )
        if status != TaskCenterTaskStatus.WAITING_WORKFLOW.value:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r} is not waiting on a goal."
            )

        parent_task = self._runtime.parent_task_for_delegated_workflow(
            task_id=report.requested_by_task_id, attempt_id=attempt_id
        )
        if parent_task is None:
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
        parent_task.apply_workflow_closure_report(report)
        return WorkflowClosureDeliveryResult(
            status="delivered",
            requested_by_task_id=report.requested_by_task_id,
            parent_attempt_id=attempt_id,
        )

    def _deliver_entry_origin(self, report: WorkflowClosureReport) -> WorkflowClosureDeliveryResult:
        run = self._runtime.task_store.get_run(report.task_center_run_id)
        if run is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter run {report.task_center_run_id!r} was not found."
            )
        if run.get("status") in ("done", "failed"):
            return WorkflowClosureDeliveryResult(
                status="already_delivered",
                requested_by_task_id=None,
                parent_attempt_id=None,
            )
        self._runtime.task_store.finish_run(
            report.task_center_run_id,
            status="done" if report.outcome == "success" else "failed",
        )
        return WorkflowClosureDeliveryResult(
            status="delivered",
            requested_by_task_id=None,
            parent_attempt_id=None,
        )
