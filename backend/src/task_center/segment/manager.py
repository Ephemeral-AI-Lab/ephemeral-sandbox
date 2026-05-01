"""TaskSegmentManager — per-segment retry and closure-report emitter.

Sole creator of HarnessGraph records inside its owned segment, and the only
emitter of ``TaskSegmentClosureReport``.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime
from typing import TYPE_CHECKING

from db.stores.harness_graph_store import HarnessGraphStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.task_segment_store import TaskSegmentStore
from task_center.exceptions import GraphInvariantViolation
from task_center.harness_graph.graph import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStatus,
)
from task_center.harness_graph.validation import (
    assert_fail_reason_present_on_failure,
    assert_graph_sequence_contiguous,
)
from task_center.segment.closure_report import (
    AttemptedPlanEntry,
    AttemptPlanFailed,
    SuccessContinue,
    TaskSegmentClosureReport,
    TerminalSuccess,
)
from task_center.segment.validation import (
    assert_graph_belongs_to_segment,
    assert_segment_has_budget,
    assert_segment_open,
)
from task_center.segment.segment import TaskSegment, TaskSegmentStatus

if TYPE_CHECKING:
    from task_center.harness_graph.orchestrator import HarnessGraphOrchestrator

logger = logging.getLogger(__name__)


ClosureReportSink = Callable[[TaskSegmentClosureReport], None]
GraphClosedCallback = Callable[[str], None]
OrchestratorFactory = Callable[
    [HarnessGraph, GraphClosedCallback], "HarnessGraphOrchestrator"
]


class TaskSegmentManager:
    """Manages one open TaskSegment's lifecycle."""

    def __init__(
        self,
        *,
        task_segment_id: str,
        segment_store: TaskSegmentStore,
        graph_store: HarnessGraphStore,
        on_segment_closed: ClosureReportSink,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskCenterStore | None = None,
    ) -> None:
        self.task_segment_id = task_segment_id
        self._segment_store = segment_store
        self._graph_store = graph_store
        self._on_segment_closed = on_segment_closed
        self._orchestrator_factory = orchestrator_factory
        # Optional — when present, the manager denormalizes the evaluator's
        # pass-summary text onto the segment row at successful close so the
        # context engine's planner_v1 recipe can read it on retry / chain.
        self._task_store = task_store

    # ---- public API -----------------------------------------------------

    def create_initial_harness_graph(self, *, start: bool = True) -> HarnessGraph:
        """Create graph_sequence_no=1 and optionally start its orchestrator."""
        segment = self._current_segment_snapshot()
        assert_segment_open(segment)
        if segment.harness_graph_ids:
            raise GraphInvariantViolation(
                f"TaskSegment {segment.id!r} already has graphs; use "
                f"create_next_harness_graph"
            )
        graph = self._insert_graph(segment, graph_sequence_no=1)
        if start:
            self.start_harness_graph(graph)
        return graph

    def start_harness_graph(self, graph: HarnessGraph) -> None:
        """Start a graph that belongs to this manager's open segment."""
        segment = self._current_segment_snapshot()
        assert_segment_open(segment)
        assert_graph_belongs_to_segment(graph, segment)
        self._start_orchestrator_if_configured(graph)

    def create_next_harness_graph(
        self, *, previous_harness_graph_id: str
    ) -> HarnessGraph:
        """Called after a failed graph if the segment still has budget."""
        segment = self._current_segment_snapshot()
        assert_segment_open(segment)
        assert_segment_has_budget(segment)
        if segment.latest_graph_id != previous_harness_graph_id:
            raise GraphInvariantViolation(
                f"previous_harness_graph_id {previous_harness_graph_id!r} is not "
                f"the latest graph of segment {segment.id!r} "
                f"(latest={segment.latest_graph_id!r})"
            )
        graph = self._insert_graph(
            segment, graph_sequence_no=segment.attempt_count + 1
        )
        self._start_orchestrator_if_configured(graph)
        return graph

    def handle_harness_graph_closed(self, harness_graph_id: str) -> None:
        """Entry point for the closed-graph callback from the orchestrator."""
        graph = self._graph_store.get(harness_graph_id)
        if graph is None:
            raise GraphInvariantViolation(
                f"HarnessGraph {harness_graph_id!r} not found"
            )
        segment = self._current_segment_snapshot()
        assert_segment_open(segment)
        assert_graph_belongs_to_segment(graph, segment)
        assert_fail_reason_present_on_failure(graph)

        if graph.status == HarnessGraphStatus.PASSED:
            self._close_segment_passed(graph)
        else:
            self._retry_or_close_failed(graph)

    # ---- internal -------------------------------------------------------

    def _current_segment_snapshot(self) -> TaskSegment:
        segment = self._segment_store.get(self.task_segment_id)
        if segment is None:
            raise GraphInvariantViolation(
                f"TaskSegment {self.task_segment_id!r} not found"
            )
        return segment

    def _insert_graph(
        self, segment: TaskSegment, *, graph_sequence_no: int
    ) -> HarnessGraph:
        assert_graph_sequence_contiguous(segment, graph_sequence_no)
        graph = self._graph_store.insert(
            task_segment_id=segment.id,
            graph_sequence_no=graph_sequence_no,
        )
        self._segment_store.append_graph_id(segment.id, graph.id)
        return graph

    def _start_orchestrator_if_configured(self, graph: HarnessGraph) -> None:
        if self._orchestrator_factory is None:
            return
        try:
            orchestrator = self._orchestrator_factory(
                graph, self.handle_harness_graph_closed
            )
            orchestrator.start()
        except Exception:
            self._close_graph_after_startup_failure(graph)
            raise

    def _close_graph_after_startup_failure(self, graph: HarnessGraph) -> None:
        try:
            latest = self._graph_store.get(graph.id)
            if latest is None or latest.is_closed:
                return
            self._graph_store.close(
                graph.id,
                status=HarnessGraphStatus.FAILED,
                fail_reason=HarnessGraphFailReason.STARTUP_FAILED,
                closed_at=datetime.now(UTC),
            )
        except Exception:
            logger.exception(
                "TaskSegmentManager: startup graph cleanup failed",
            )

    def _close_segment_passed(self, graph: HarnessGraph) -> None:
        self._segment_store.set_continuation_goal(
            self.task_segment_id, graph.continuation_goal
        )
        # Atomically transition status + write the denormalized
        # task_specification (from the passing graph) and task_summary
        # (from the evaluator's pass summary text) onto the segment row.
        self._segment_store.close_succeeded(
            self.task_segment_id,
            task_specification=graph.task_specification or "",
            task_summary=self._evaluator_pass_summary_for(graph),
            closed_at=datetime.now(UTC),
        )
        if graph.continuation_goal is None:
            self._emit_terminal_success(graph)
        else:
            self._emit_success_continue(graph)

    def _evaluator_pass_summary_for(self, graph: HarnessGraph) -> str:
        """Resolve the evaluator's success-summary text for *graph*.

        Empty string when the manager is configured without a ``task_store``
        (test seams) or when the evaluator never recorded a summary.
        """
        if self._task_store is None:
            return ""
        return self._task_store.get_evaluator_pass_summary(graph.id)

    def _retry_or_close_failed(self, graph: HarnessGraph) -> None:
        segment = self._current_segment_snapshot()
        if not segment.has_budget_remaining:
            self._close_segment_failed(graph)
            return
        try:
            self.create_next_harness_graph(previous_harness_graph_id=graph.id)
        except Exception:
            # Retry start failed; the new graph was inserted and closed
            # STARTUP_FAILED before the exception propagated. Re-enter the
            # retry decision on the new closed graph instead of leaving the
            # segment open.
            retry_graph = self._latest_failed_graph_for(previous_id=graph.id)
            if retry_graph is None:
                raise
            logger.warning(
                "TaskSegmentManager: retry start failure for segment %r; "
                "treating new graph %r as a failed attempt",
                self.task_segment_id,
                retry_graph.id,
                exc_info=True,
            )
            self._retry_or_close_failed(retry_graph)

    def _close_segment_failed(self, graph: HarnessGraph) -> None:
        self._segment_store.set_status(
            self.task_segment_id,
            status=TaskSegmentStatus.FAILED,
            closed_at=datetime.now(UTC),
        )
        self._emit_attempt_plan_failed(graph)

    def _latest_failed_graph_for(
        self, *, previous_id: str
    ) -> HarnessGraph | None:
        segment = self._current_segment_snapshot()
        latest_id = segment.latest_graph_id
        if latest_id is None or latest_id == previous_id:
            return None
        retry_graph = self._graph_store.get(latest_id)
        if retry_graph is None or retry_graph.status != HarnessGraphStatus.FAILED:
            return None
        return retry_graph

    def _emit_terminal_success(self, graph: HarnessGraph) -> None:
        report = TaskSegmentClosureReport(
            task_segment_id=self.task_segment_id,
            final_harness_graph_id=graph.id,
            outcome=TerminalSuccess(),
        )
        self._on_segment_closed(report)

    def _emit_success_continue(self, graph: HarnessGraph) -> None:
        if graph.continuation_goal is None:
            raise GraphInvariantViolation(
                "success_continue requires a non-null continuation_goal"
            )
        report = TaskSegmentClosureReport(
            task_segment_id=self.task_segment_id,
            final_harness_graph_id=graph.id,
            outcome=SuccessContinue(goal=graph.continuation_goal),
        )
        self._on_segment_closed(report)

    def _emit_attempt_plan_failed(self, last_graph: HarnessGraph) -> None:
        history = self._build_attempted_plan_history()
        report = TaskSegmentClosureReport(
            task_segment_id=self.task_segment_id,
            final_harness_graph_id=last_graph.id,
            outcome=AttemptPlanFailed(
                failure_summary=(
                    last_graph.fail_reason.value
                    if last_graph.fail_reason is not None
                    else "unknown"
                ),
                attempted_plan_history=history,
            ),
        )
        self._on_segment_closed(report)

    def _build_attempted_plan_history(self) -> tuple[AttemptedPlanEntry, ...]:
        graphs = self._graph_store.list_for_segment(self.task_segment_id)
        return tuple(
            AttemptedPlanEntry(
                harness_graph_id=g.id,
                graph_sequence_no=g.graph_sequence_no,
                task_specification=g.task_specification,
                evaluation_criteria=g.evaluation_criteria,
                fail_reason=g.fail_reason,
                harness_graph_summary_id=None,
                failure_landscape=None,
            )
            for g in graphs
        )
