"""MissionRequestStarter — use-case boundary for delegated request start.

Composes the existing request, segment, manager, and parent-task owners into
the single safe mission-start path used by ``request_complex_task_solution``. Owns
parent-task CAS, deferred orchestrator startup, and compensation on failure.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import UTC, datetime

from task_center.mission.close_report_delivery import (
    ComplexTaskCloseReportRouter,
)
from task_center.mission.handler import ComplexTaskRequestHandler
from task_center.mission.mission import (
    ComplexTaskCloseReport,
    ComplexTaskRequest,
)
from task_center.exceptions import GraphInvariantViolation
from task_center.attempt.factory import (
    make_attempt_orchestrator_factory,
)
from task_center.attempt import HarnessGraphFailReason, HarnessGraphStatus
from task_center.attempt.runtime import HarnessGraphRuntime
from task_center.episode.episode import TaskSegment
from task_center.task import HarnessTaskStatus

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class StartedMissionRequest:
    parent_task_id: str
    # ``None`` when the caller is the graph-less entry executor.
    parent_harness_graph_id: str | None
    complex_task_request_id: str
    initial_segment_id: str
    initial_harness_graph_id: str
    goal: str


class MissionRequestStarter:
    """Single orchestration entry point for executor → delegated mission start."""

    def __init__(self, *, runtime: HarnessGraphRuntime) -> None:
        self._runtime = runtime
        self._handler: ComplexTaskRequestHandler | None = None

    def start(
        self,
        *,
        task_center_run_id: str,
        parent_task_id: str,
        parent_harness_graph_id: str | None,
        goal: str,
    ) -> StartedMissionRequest:
        self._assert_parent_running_and_no_open_child(
            parent_task_id=parent_task_id,
            parent_harness_graph_id=parent_harness_graph_id,
        )

        handler = self._build_handler()
        delegated_request = handler.create_mission_request(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
            goal=goal,
        )
        (
            initial_segment,
            segment_manager,
        ) = handler.create_initial_episode_with_manager(
            complex_task_request_id=delegated_request.id,
        )

        initial_graph = None
        try:
            initial_graph = segment_manager.create_initial_attempt(start=False)
            self._mark_parent_waiting(
                parent_task_id=parent_task_id,
                parent_harness_graph_id=parent_harness_graph_id,
                request=delegated_request,
                segment=initial_segment,
                graph_id=initial_graph.id,
                goal=goal,
            )
            segment_manager.start_attempt(initial_graph)
        except Exception:
            self._compensate_failed_start(
                request=delegated_request,
                segment=initial_segment,
                initial_graph_id=(
                    initial_graph.id if initial_graph is not None else None
                ),
                parent_task_id=parent_task_id,
            )
            raise

        assert initial_graph is not None
        return StartedMissionRequest(
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
                "MissionRequestStarter requires a segment manager registry."
            )
        router = ComplexTaskCloseReportRouter(runtime=self._runtime)

        def _deliver(report: ComplexTaskCloseReport) -> None:
            router.deliver(report)

        orchestrator_factory = make_attempt_orchestrator_factory(
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
        parent_harness_graph_id: str | None,
    ) -> None:
        task = self._runtime.task_store.get_task(parent_task_id)
        if task is None:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} was not found."
            )
        if task.get("status") != HarnessTaskStatus.RUNNING.value:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} is not running; "
                "delegated mission start requires a running generator task."
            )
        attached_graph = str(task.get("task_center_harness_graph_id") or "")
        # In entry mode the caller has no parent graph (parent_harness_graph_id
        # is None) and the task row's graph id column is empty/None too. In
        # graph mode both must match.
        expected = parent_harness_graph_id or ""
        if attached_graph != expected:
            raise GraphInvariantViolation(
                f"TaskCenter task {parent_task_id!r} is attached to graph "
                f"{attached_graph!r}, not {expected!r}."
            )
        # Entry-mode caveat: the entry task's *own* complex_request has
        # ``requested_by_task_id == entry_task_id`` because the entry task
        # is the top-level requestor. That self-request is not a child and
        # must be excluded from the duplicate-open-child check.
        controller = self._runtime.entry_task_controller_for(parent_task_id)
        own_request_id = (
            controller.complex_task_request_id if controller is not None else None
        )
        existing_open = [
            r
            for r in self._runtime.request_store.list_for_executor_task(
                parent_task_id
            )
            if r.is_open and r.id != own_request_id
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
        parent_harness_graph_id: str | None,
        request: ComplexTaskRequest,
        segment: TaskSegment,
        graph_id: str,
        goal: str,
    ) -> None:
        # Entry-mode caller: route through the EntryTaskController so the
        # controller is the single owner of entry-task state transitions.
        controller = self._runtime.entry_task_controller_for(parent_task_id)
        if controller is not None:
            controller.mark_waiting_complex_task(
                delegated_request_id=request.id,
                delegated_segment_id=segment.id,
                delegated_graph_id=graph_id,
                goal=goal,
            )
            return

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
                "delegated mission start tried to mark it waiting."
            )

    def _compensate_failed_start(
        self,
        *,
        request: ComplexTaskRequest,
        segment: TaskSegment,
        initial_graph_id: str | None,
        parent_task_id: str,
    ) -> None:
        """Best-effort rollback. Order: graph → segment → request → parent."""
        now = datetime.now(UTC)
        self._close_unstarted_attempt_after_failed_start(initial_graph_id, now=now)
        try:
            self._runtime.segment_store.cancel_for_compensation(
                segment.id, closed_at=now
            )
        except Exception:
            logger.exception(
                "MissionRequestStarter: cancel segment failed",
            )
        try:
            self._runtime.request_store.cancel_for_compensation(
                request.id, closed_at=now
            )
        except Exception:
            logger.exception(
                "MissionRequestStarter: cancel request failed",
            )
        try:
            controller = self._runtime.entry_task_controller_for(parent_task_id)
            if controller is not None:
                # Entry-mode rollback flows through the controller so the
                # controller stays the single owner of entry-task transitions.
                controller.restore_running_after_failed_mission_start()
            else:
                self._runtime.task_store.set_task_status_if_current(
                    parent_task_id,
                    expected_status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
                    status=HarnessTaskStatus.RUNNING.value,
                )
        except Exception:
            logger.critical(
                "MissionRequestStarter: parent status rollback failed; "
                "task %r will remain in WAITING_COMPLEX_TASK and requires "
                "manual recovery",
                parent_task_id,
                exc_info=True,
            )
        manager_registry = self._runtime.manager_registry
        if manager_registry is not None:
            manager_registry.deregister(segment.id)

    def _close_unstarted_attempt_after_failed_start(
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
                "MissionRequestStarter: failed to close attempt "
                "after mission-start failure",
            )
