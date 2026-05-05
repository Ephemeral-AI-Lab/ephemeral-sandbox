"""ComplexTaskRequestHandler — request boundary lifecycle service.

Only creator of ``ComplexTaskRequest`` and ``TaskSegment`` records, and the
spawner of ``TaskSegmentManager`` instances. Routes ``TaskSegmentClosureReport``
into either continuation segment creation or request closure.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import UTC, datetime
from typing import Literal

from db.stores.complex_task_request_store import ComplexTaskRequestStore
from db.stores.harness_graph_store import HarnessGraphStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.task_segment_store import TaskSegmentStore
from task_center.mission.validation import (
    assert_continuation_episode_predecessor,
    assert_mission_request_open,
    assert_episode_id_unique_in_mission,
    assert_episode_sequence_contiguous,
)
from task_center.mission.mission import (
    ComplexTaskCloseReport,
    ComplexTaskRequest,
    ComplexTaskRequestStatus,
)
from task_center.config import HarnessLifecycleConfig
from task_center.exceptions import GraphInvariantViolation
from task_center.episode.closure_report import (
    AttemptPlanFailed,
    SuccessContinue,
    TaskSegmentClosureReport,
    TerminalSuccess,
)
from task_center.episode.manager import OrchestratorFactory, TaskSegmentManager
from task_center.episode.registry import SegmentManagerRegistry
from task_center.episode.episode import (
    TaskSegment,
    TaskSegmentCreationReason,
)


CloseReportSink = Callable[[ComplexTaskCloseReport], None]


class ComplexTaskRequestHandler:
    """Owns the request boundary: request + segment creation, request closure."""

    def __init__(
        self,
        *,
        request_store: ComplexTaskRequestStore,
        segment_store: TaskSegmentStore,
        graph_store: HarnessGraphStore,
        manager_registry: SegmentManagerRegistry,
        config: HarnessLifecycleConfig,
        deliver_close_report: CloseReportSink | None = None,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskCenterStore | None = None,
    ) -> None:
        self._request_store = request_store
        self._segment_store = segment_store
        self._graph_store = graph_store
        self._manager_registry = manager_registry
        self._config = config
        self._deliver_close_report = deliver_close_report
        self._orchestrator_factory = orchestrator_factory
        self._task_store = task_store

    # ---- public API -----------------------------------------------------

    def set_orchestrator_factory(
        self, factory: OrchestratorFactory
    ) -> None:
        """Inject the orchestrator factory after construction.

        The entry coordinator builds the handler before the runtime exists
        (the handler is a dependency of the entry-task controller, which is
        in turn a dependency of the runtime). The factory closes over the
        runtime, so it must be installed once construction completes.
        """
        self._orchestrator_factory = factory

    def create_mission_request(
        self,
        *,
        task_center_run_id: str,
        requested_by_task_id: str,
        goal: str,
    ) -> ComplexTaskRequest:
        return self._request_store.insert(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=requested_by_task_id,
            goal=goal,
        )

    def create_initial_episode(
        self, *, complex_task_request_id: str
    ) -> TaskSegment:
        segment, _ = self.create_initial_episode_with_manager(
            complex_task_request_id=complex_task_request_id
        )
        return segment

    def create_initial_episode_with_manager(
        self, *, complex_task_request_id: str
    ) -> tuple[TaskSegment, TaskSegmentManager]:
        request = self._require_request(complex_task_request_id)
        assert_mission_request_open(request)
        assert_episode_sequence_contiguous(request, new_sequence_no=1)
        segment = self._segment_store.insert(
            complex_task_request_id=complex_task_request_id,
            sequence_no=1,
            creation_reason=TaskSegmentCreationReason.INITIAL,
            goal=request.goal,
            attempt_budget=self._config.default_attempt_budget,
        )
        self._append_episode_to_mission(request, segment)
        manager = self._spawn_episode_manager(segment)
        return segment, manager

    def create_continuation_episode(
        self, *, previous_segment: TaskSegment
    ) -> TaskSegment:
        segment, _ = self.create_continuation_episode_with_manager(
            previous_segment=previous_segment
        )
        return segment

    def create_continuation_episode_with_manager(
        self, *, previous_segment: TaskSegment
    ) -> tuple[TaskSegment, TaskSegmentManager]:
        request = self._require_request(previous_segment.complex_task_request_id)
        assert_mission_request_open(request)
        assert_continuation_episode_predecessor(previous_segment)
        new_sequence_no = previous_segment.sequence_no + 1
        assert_episode_sequence_contiguous(request, new_sequence_no=new_sequence_no)
        # Narrowed by the invariant above.
        assert previous_segment.continuation_goal is not None
        segment = self._segment_store.insert(
            complex_task_request_id=request.id,
            sequence_no=new_sequence_no,
            creation_reason=TaskSegmentCreationReason.PARTIAL_CONTINUATION,
            goal=previous_segment.continuation_goal,
            attempt_budget=self._config.default_attempt_budget,
        )
        self._append_episode_to_mission(request, segment)
        manager = self._spawn_episode_manager(segment)
        return segment, manager

    def handle_episode_closed(
        self, report: TaskSegmentClosureReport
    ) -> None:
        segment = self._segment_store.get(report.task_segment_id)
        if segment is None:
            raise GraphInvariantViolation(
                f"TaskSegment {report.task_segment_id!r} not found"
            )
        try:
            outcome = report.outcome
            if isinstance(outcome, SuccessContinue):
                (
                    next_segment,
                    next_manager,
                ) = self.create_continuation_episode_with_manager(
                    previous_segment=segment,
                )
                self._start_continuation_episode(
                    next_segment=next_segment,
                    next_manager=next_manager,
                    previous_report=report,
                )
            elif isinstance(outcome, TerminalSuccess):
                self.close_mission_request(
                    complex_task_request_id=segment.complex_task_request_id,
                    succeeded=True,
                    final_segment_id=segment.id,
                    final_harness_graph_id=report.final_harness_graph_id,
                )
            elif isinstance(outcome, AttemptPlanFailed):
                self.close_mission_request(
                    complex_task_request_id=segment.complex_task_request_id,
                    succeeded=False,
                    final_segment_id=segment.id,
                    final_harness_graph_id=report.final_harness_graph_id,
                )
            else:  # pragma: no cover - exhaustive over discriminated union
                raise GraphInvariantViolation(
                    f"Unknown ClosureOutcome: {outcome!r}"
                )
        finally:
            self._manager_registry.deregister(segment.id)

    def close_mission_request(
        self,
        *,
        complex_task_request_id: str,
        succeeded: bool,
        final_segment_id: str,
        final_harness_graph_id: str | None,
    ) -> ComplexTaskRequest:
        request = self._require_request(complex_task_request_id)
        assert_mission_request_open(request)
        outcome_label: Literal["success", "failed"] = (
            "success" if succeeded else "failed"
        )
        close_report = ComplexTaskCloseReport(
            complex_task_request_id=complex_task_request_id,
            requested_by_task_id=request.requested_by_task_id,
            outcome=outcome_label,
            final_segment_id=final_segment_id,
            final_harness_graph_id=final_harness_graph_id,
        )
        status = (
            ComplexTaskRequestStatus.SUCCEEDED
            if succeeded
            else ComplexTaskRequestStatus.FAILED
        )
        updated = self._request_store.set_status(
            complex_task_request_id,
            status=status,
            final_outcome=close_report.to_final_outcome(),
            closed_at=datetime.now(UTC),
        )
        if self._deliver_close_report is not None:
            self._deliver_close_report(close_report)
        return updated

    # ---- internal -------------------------------------------------------

    def _start_continuation_episode(
        self,
        *,
        next_segment: TaskSegment,
        next_manager: TaskSegmentManager,
        previous_report: TaskSegmentClosureReport,
    ) -> None:
        """Create and start the continuation segment's initial graph.

        Skipped when no ``orchestrator_factory`` is configured: in that case
        the test or harness driver is responsible for creating and stopping the
        graph manually. Production paths always attach a factory through the
        mission starter, so continuation startup runs end-to-end.

        On startup failure the continuation segment is cancelled and the
        request is closed as failed. If graph insertion already happened, the
        close report points at that failed continuation graph.
        """
        if self._orchestrator_factory is None:
            return
        try:
            next_manager.create_initial_attempt()
        except Exception:
            failed_graph_id = self._latest_attempt_id_for_episode(
                next_segment.id
            ) or previous_report.final_harness_graph_id
            self._segment_store.cancel_for_compensation(
                next_segment.id, closed_at=datetime.now(UTC)
            )
            self._manager_registry.deregister(next_segment.id)
            self.close_mission_request(
                complex_task_request_id=next_segment.complex_task_request_id,
                succeeded=False,
                final_segment_id=next_segment.id,
                final_harness_graph_id=failed_graph_id,
            )

    def _require_request(self, request_id: str) -> ComplexTaskRequest:
        request = self._request_store.get(request_id)
        if request is None:
            raise GraphInvariantViolation(
                f"ComplexTaskRequest {request_id!r} not found"
            )
        return request

    def _append_episode_to_mission(
        self, request: ComplexTaskRequest, segment: TaskSegment
    ) -> None:
        assert_episode_id_unique_in_mission(request, segment.id)
        self._request_store.append_segment_id(request.id, segment.id)

    def _latest_attempt_id_for_episode(self, segment_id: str) -> str | None:
        segment = self._segment_store.get(segment_id)
        if segment is None:
            return None
        return segment.latest_graph_id

    def _spawn_episode_manager(self, segment: TaskSegment) -> TaskSegmentManager:
        manager = TaskSegmentManager(
            task_segment_id=segment.id,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
            on_segment_closed=self.handle_episode_closed,
            orchestrator_factory=self._orchestrator_factory,
            task_store=self._task_store,
        )
        self._manager_registry.register(manager)
        return manager
