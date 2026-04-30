"""ComplexTaskCloseReport delivery router and replay helpers.

Owns the single delivery path from ``ComplexTaskRequestHandler.close_complex_task_request``
to the parent ``HarnessGraphOrchestrator.apply_complex_task_close_report``.
Replay helpers reconstruct reports from durable ``final_outcome`` rows so a
report dropped during a handoff is delivered the next time the parent
orchestrator is active.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from task_center.complex_task.request import (
    ComplexTaskCloseReport,
    ComplexTaskRequest,
)
from task_center.exceptions import GraphInvariantViolation
from task_center.harness_graph.runtime import HarnessGraphRuntime
from task_center.task import HarnessTaskStatus

CloseReportDeliveryStatus = Literal[
    "delivered",
    "already_delivered",
    "deferred_no_orchestrator",
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
            return CloseReportDeliveryResult(
                status="deferred_no_orchestrator",
                requested_by_task_id=report.requested_by_task_id,
                parent_harness_graph_id=graph_id,
            )
        orchestrator.apply_complex_task_close_report(report)
        return CloseReportDeliveryResult(
            status="delivered",
            requested_by_task_id=report.requested_by_task_id,
            parent_harness_graph_id=graph_id,
        )


def build_close_report_from_request(
    request: ComplexTaskRequest,
) -> ComplexTaskCloseReport | None:
    """Reconstruct a close report from a closed request's ``final_outcome``.

    Returns ``None`` for any request that did not end with a delivered outcome
    (open, or cancelled by handoff compensation).
    """
    try:
        return ComplexTaskCloseReport.from_request(request)
    except ValueError as exc:
        raise GraphInvariantViolation(str(exc)) from exc


def deliver_pending_complex_task_close_reports(
    *,
    runtime: HarnessGraphRuntime,
    task_center_run_id: str | None = None,
) -> list[CloseReportDeliveryResult]:
    """Replay closed ``ComplexTaskRequest``s whose parent task is still waiting.

    When ``task_center_run_id`` is None, scans every closed request the store
    knows about. Idempotent: already-delivered reports return
    ``already_delivered`` rather than re-mutating the parent.
    """
    if task_center_run_id is None:
        closed = runtime.request_store.list_closed()
    else:
        closed = runtime.request_store.list_closed_for_run(task_center_run_id)
    router = ComplexTaskCloseReportRouter(runtime=runtime)
    results: list[CloseReportDeliveryResult] = []
    for request in closed:
        report = build_close_report_from_request(request)
        if report is None:
            continue
        results.append(router.deliver(report))
    return results
