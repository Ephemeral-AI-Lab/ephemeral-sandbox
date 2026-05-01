"""ComplexTaskCloseReport delivery router.

Owns the single delivery path from ``ComplexTaskRequestHandler.close_complex_task_request``
to the parent ``HarnessGraphOrchestrator.apply_complex_task_close_report``.

The runtime assumes no process restart: while a parent generator task is in
``WAITING_COMPLEX_TASK`` its graph cannot reach quiescence and its
orchestrator stays registered. Therefore close-report delivery is always
synchronous against an active parent orchestrator; a missing orchestrator at
delivery time is a hard ``GraphInvariantViolation``.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from task_center.complex_task.request import ComplexTaskCloseReport
from task_center.exceptions import GraphInvariantViolation
from task_center.harness_graph.runtime import HarnessGraphRuntime
from task_center.task import HarnessTaskStatus

CloseReportDeliveryStatus = Literal[
    "delivered",
    "already_delivered",
]


@dataclass(frozen=True, slots=True)
class CloseReportDeliveryResult:
    status: CloseReportDeliveryStatus
    requested_by_task_id: str
    parent_harness_graph_id: str | None


class ComplexTaskCloseReportRouter:
    """Single delivery path for final ``ComplexTaskCloseReport``s."""

    def __init__(self, *, runtime: HarnessGraphRuntime) -> None:
        self._runtime = runtime

    def deliver(
        self, report: ComplexTaskCloseReport
    ) -> CloseReportDeliveryResult:
        task = self._runtime.task_store.get_task(report.requested_by_task_id)
        if task is None:
            raise GraphInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r} was not found."
            )
        graph_id = str(task.get("task_center_harness_graph_id") or "") or None
        status = str(task.get("status") or "")
        if status in (
            HarnessTaskStatus.DONE.value,
            HarnessTaskStatus.FAILED.value,
        ):
            return CloseReportDeliveryResult(
                status="already_delivered",
                requested_by_task_id=report.requested_by_task_id,
                parent_harness_graph_id=graph_id,
            )
        if status != HarnessTaskStatus.WAITING_COMPLEX_TASK.value:
            raise GraphInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r} is not waiting "
                "on a complex task."
            )
        if graph_id is None or graph_id.isspace():
            raise GraphInvariantViolation(
                f"TaskCenter task {report.requested_by_task_id!r} is not "
                "attached to a harness graph."
            )

        orchestrator = self._runtime.orchestrator_registry.get(graph_id)
        if orchestrator is None:
            raise GraphInvariantViolation(
                f"Parent HarnessGraphOrchestrator for graph {graph_id!r} is "
                "not registered; close-report delivery requires an active "
                "parent orchestrator."
            )
        orchestrator.apply_complex_task_close_report(report)
        return CloseReportDeliveryResult(
            status="delivered",
            requested_by_task_id=report.requested_by_task_id,
            parent_harness_graph_id=graph_id,
        )
