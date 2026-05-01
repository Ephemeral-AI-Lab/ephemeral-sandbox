"""ComplexTaskHandoffCoordinator — use-case boundary for delegated request start.

Composes the existing request, segment, manager, and parent-task owners into
the single safe handoff path used by ``request_complex_task_solution``. Owns
parent-task CAS, deferred orchestrator startup, and compensation on failure.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import UTC, datetime

from task_center.complex_task.close_report_delivery import (
    ComplexTaskCloseReportRouter,
)
from task_center.complex_task.handler import ComplexTaskRequestHandler
from task_center.complex_task.request import (
    ComplexTaskCloseReport,
    ComplexTaskRequest,
)
from task_center.exceptions import GraphInvariantViolation
from task_center.harness_graph.factory import (
    make_harness_graph_orchestrator_factory,
)
from task_center.harness_graph.graph import HarnessGraphFailReason, HarnessGraphStatus
from task_center.harness_graph.runtime import HarnessGraphRuntime
from task_center.segment.segment import TaskSegment
from task_center.task import HarnessTaskStatus

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class ComplexTaskHandoffResult:
    parent_task_id: str
    parent_harness_graph_id: str
    complex_task_request_id: str
    initial_segment_id: str
    initial_harness_graph_id: str
    goal: str


class ComplexTaskHandoffCoordinator:
    """Single orchestration entry point for executor → delegated request handoff."""

    def __init__(self, *, runtime: HarnessGraphRuntime) -> None:
        self._runtime = runtime
        self._handler: ComplexTaskRequestHandler | None = None

    def start(
        self,
        *,
        task_center_run_id: str,
        parent_task_id: str,
        parent_harness_graph_id: str,
        goal: str,
    ) -> ComplexTaskHandoffResult:
        self._assert_parent_running_and_no_open_child(
            parent_task_id=parent_task_id,
            parent_harness_graph_id=parent_harness_graph_id,
        )

        handler = self._build_handler()
        delegated_request = handler.create_complex_task_request(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
            goal=goal,
        )
        (
            initial_segment,
            segment_manager,
        ) = handler.create_initial_segment_with_manager(
            complex_task_request_id=delegated_request.id,
        )

        initial_graph = None
        try:
            initial_graph = segment_manager.create_initial_harness_graph(start=False)
            self._mark_parent_waiting(
                parent_task_id=parent_task_id,
                parent_harness_graph_id=parent_harness_graph_id,
                request=delegated_request,
                segment=initial_segment,
                graph_id=initial_graph.id,
                goal=goal,
            )
            segment_manager.start_harness_graph(initial_graph)
        except Exception:
            self._compensate_failed_handoff(
                request=delegated_request,
                segment=initial_segment,
                initial_graph_id=(
                    initial_graph.id if initial_graph is not None else None
                ),
                parent_task_id=parent_task_id,
            )
            raise

        assert initial_graph is not None
        return ComplexTaskHandoffResult(
            parent_task_id=parent_task_id,
            parent_harness_graph_id=parent_harness_graph_id,
            complex_task_request_id=delegated_request.id,
            initial_segment_id=initial_segment.id,
            initial_harness_graph_id=initial_graph.id,
            goal=goal,
        )

    # ---- internal -------------------------------------------------------

    def _build_handler(self) -> ComplexTaskRequestHandler:
        if self._handler is not None:
            return self._handler
        manager_registry = self._runtime.manager_registry
        if manager_registry is None:
            raise GraphInvariantViolation(
                "ComplexTaskHandoffCoordinator requires a segment manager registry."
            )
        router = ComplexTaskCloseReportRouter(runtime=self._runtime)

        def _deliver(report: ComplexTaskCloseReport) -> None:
            router.deliver(report)

        orchestrator_factory = make_harness_graph_orchestrator_factory(
            runtime=self._runtime,
        )
        self._handler = ComplexTaskRequestHandler(
            request_store=self._runtime.request_store,
            segment_store=self._runtime.segment_store,
            graph_store=self._runtime.graph_store,
            manager_registry=manager_registry,
            config=self._runtime.lifecycle_config,
            deliver_close_report=_deliver,
            orchestrator_factory=orchestrator_factory,
        )
        return self._handler

    def _assert_parent_running_and_no_open_child(
        self,
        *,
        parent_task_id: str,
        parent_harness_graph_id: str,
    ) -> None:
        task = self._runtime.task_store.get_task(parent_task_id)
        if task is None:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} was not found."
            )
        if task.get("status") != HarnessTaskStatus.RUNNING.value:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} is not running; "
                "complex-task handoff requires a running generator task."
            )
        attached_graph = str(task.get("task_center_harness_graph_id") or "")
        if attached_graph != parent_harness_graph_id:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} is attached to graph "
                f"{attached_graph!r}, not {parent_harness_graph_id!r}."
            )
        existing_open = [
            r
            for r in self._runtime.request_store.list_for_executor_task(
                parent_task_id
            )
            if r.is_open
        ]
        if existing_open:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} already has an open "
                f"complex-task request {existing_open[0].id!r}."
            )

    def _mark_parent_waiting(
        self,
        *,
        parent_task_id: str,
        parent_harness_graph_id: str,
        request: ComplexTaskRequest,
        segment: TaskSegment,
        graph_id: str,
        goal: str,
    ) -> None:
        summary = {
            "outcome": "complex_task_request_start",
            "summary": "Waiting on delegated complex task solution.",
            "payload": {
                "complex_task_request_id": request.id,
                "initial_segment_id": segment.id,
                "initial_harness_graph_id": graph_id,
                "parent_harness_graph_id": parent_harness_graph_id,
                "goal": goal,
            },
        }
        updated = self._runtime.task_store.set_task_status_if_current(
            parent_task_id,
            expected_status=HarnessTaskStatus.RUNNING.value,
            status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
            summary=summary,
        )
        if updated is None:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} was not running when the "
                "complex-task handoff tried to mark it waiting."
            )

    def _compensate_failed_handoff(
        self,
        *,
        request: ComplexTaskRequest,
        segment: TaskSegment,
        initial_graph_id: str | None,
        parent_task_id: str,
    ) -> None:
        """Best-effort rollback. Order: graph → segment → request → parent."""
        now = datetime.now(UTC)
        self._close_unstarted_graph_after_failed_handoff(initial_graph_id, now=now)
        try:
            self._runtime.segment_store.cancel_for_compensation(
                segment.id, closed_at=now
            )
        except Exception:
            logger.exception(
                "ComplexTaskHandoffCoordinator: cancel segment failed",
            )
        try:
            self._runtime.request_store.cancel_for_compensation(
                request.id, closed_at=now
            )
        except Exception:
            logger.exception(
                "ComplexTaskHandoffCoordinator: cancel request failed",
            )
        try:
            self._runtime.task_store.set_task_status_if_current(
                parent_task_id,
                expected_status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
                status=HarnessTaskStatus.RUNNING.value,
            )
        except Exception:
            logger.critical(
                "ComplexTaskHandoffCoordinator: parent status rollback failed; "
                "task %r will remain in WAITING_COMPLEX_TASK and requires "
                "manual recovery",
                parent_task_id,
                exc_info=True,
            )
        manager_registry = self._runtime.manager_registry
        if manager_registry is not None:
            manager_registry.deregister(segment.id)

    def _close_unstarted_graph_after_failed_handoff(
        self, graph_id: str | None, *, now: datetime
    ) -> None:
        if graph_id is None:
            return
        try:
            graph = self._runtime.graph_store.get(graph_id)
            if graph is None or graph.is_closed:
                return
            self._runtime.graph_store.close(
                graph_id,
                status=HarnessGraphStatus.FAILED,
                fail_reason=HarnessGraphFailReason.STARTUP_FAILED,
                closed_at=now,
            )
        except Exception:
            logger.exception(
                "ComplexTaskHandoffCoordinator: failed to close graph "
                "after handoff failure",
            )
