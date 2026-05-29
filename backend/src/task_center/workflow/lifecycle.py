"""Workflow lifecycle coordination.

``WorkflowLifecycle`` is the entry point for creating a goal, extending its
iteration chain, and closing it. Persistence, iteration creation, and
continuation routing stay in this module behind the public lifecycle class.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.invariants import (
    assert_predecessor_has_deferred_goal_for_next_iteration,
    assert_workflow_open,
    assert_iteration_id_unique_in_workflow,
    assert_iteration_sequence_contiguous,
)
from task_center._core.persistence import (
    AttemptStoreProtocol,
    WorkflowStoreProtocol,
    IterationStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.primitives import (
    TaskCenterInvariantViolation,
    TaskCenterLifecycleConfig,
)
from task_center.workflow.state import Workflow, WorkflowClosureReport, WorkflowOrigin, WorkflowStatus
from task_center.iteration import (
    IterationAttemptCoordinator,
    OpenIterationCoordinatorRegistry,
    OrchestratorFactory,
)
from task_center.iteration.state import (
    AttemptPlanFailed,
    Iteration,
    IterationClosureReport,
    IterationCreationReason,
    IterationStatus,
    SuccessDeferred,
    TerminalSuccess,
)

logger = logging.getLogger(__name__)


WorkflowClosureCallback = Callable[[WorkflowClosureReport], object]


class WorkflowLifecycle:
    """Coordinates one goal's iteration chain and closure report delivery."""

    def __init__(
        self,
        *,
        workflow_store: WorkflowStoreProtocol,
        iteration_store: IterationStoreProtocol,
        attempt_store: AttemptStoreProtocol,
        iteration_coordinators: OpenIterationCoordinatorRegistry,
        config: TaskCenterLifecycleConfig,
        deliver_closure_report: WorkflowClosureCallback | None = None,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self._deliver_closure_report = deliver_closure_report
        self._workflow_store = workflow_store
        self._iteration_store = iteration_store
        self._attempt_store = attempt_store
        self._iteration_coordinators = iteration_coordinators
        self._config = config
        self._orchestrator_factory = orchestrator_factory
        self._task_store = task_store

    def create_workflow(
        self,
        *,
        task_center_run_id: str,
        origin: WorkflowOrigin,
        goal: str,
    ) -> Workflow:
        return self._workflow_store.insert(
            task_center_run_id=task_center_run_id,
            origin=origin,
            goal=goal,
        )

    def create_initial_iteration_with_coordinator(
        self, *, workflow_id: str
    ) -> tuple[Iteration, IterationAttemptCoordinator]:
        goal = self._require_workflow(workflow_id)
        assert_workflow_open(goal)
        assert_iteration_sequence_contiguous(goal, new_sequence_no=1)
        return self._insert_iteration_and_register_coordinator(
            goal=goal,
            sequence_no=1,
            creation_reason=IterationCreationReason.INITIAL,
            iteration_goal=goal.goal,
        )

    def create_deferred_iteration_with_coordinator(
        self, *, previous_iteration: Iteration
    ) -> tuple[Iteration, IterationAttemptCoordinator]:
        goal = self._require_workflow(previous_iteration.workflow_id)
        assert_workflow_open(goal)
        assert_predecessor_has_deferred_goal_for_next_iteration(previous_iteration)
        deferred_goal = previous_iteration.deferred_goal_for_next_iteration
        if deferred_goal is None:  # pragma: no cover - guarded by invariant above
            raise TaskCenterInvariantViolation(
                f"Iteration {previous_iteration.id!r} has no deferred goal"
            )
        new_sequence_no = previous_iteration.sequence_no + 1
        assert_iteration_sequence_contiguous(goal, new_sequence_no=new_sequence_no)
        return self._insert_iteration_and_register_coordinator(
            goal=goal,
            sequence_no=new_sequence_no,
            creation_reason=IterationCreationReason.DEFERRED_GOAL_CONTINUATION,
            iteration_goal=deferred_goal,
        )

    def handle_iteration_closed(self, report: IterationClosureReport) -> None:
        self._route_iteration_closure(report)

    def close_workflow(
        self,
        *,
        workflow_id: str,
        succeeded: bool,
        final_iteration_id: str,
        final_attempt_id: str | None,
    ) -> Workflow:
        goal = self._require_workflow(workflow_id)
        assert_workflow_open(goal)
        report = WorkflowClosureReport(
            workflow_id=workflow_id,
            task_center_run_id=goal.task_center_run_id,
            origin_kind=goal.origin_kind,
            requested_by_task_id=goal.requested_by_task_id,
            outcome="success" if succeeded else "failed",
            final_iteration_id=final_iteration_id,
            final_attempt_id=final_attempt_id,
        )
        updated = self._workflow_store.set_status(
            workflow_id,
            status=WorkflowStatus.SUCCEEDED if succeeded else WorkflowStatus.FAILED,
            final_outcome=report.to_final_outcome(),
            closed_at=datetime.now(UTC),
        )
        if self._deliver_closure_report is not None:
            self._deliver_closure_report(report)
        return updated

    def _require_workflow(self, workflow_id: str) -> Workflow:
        goal = self._workflow_store.get(workflow_id)
        if goal is None:
            raise TaskCenterInvariantViolation(f"Workflow {workflow_id!r} not found")
        return goal

    def _append_iteration_id(self, goal: Workflow, iteration_id: str) -> Workflow:
        assert_iteration_id_unique_in_workflow(goal, iteration_id)
        return self._workflow_store.append_iteration_id(goal.id, iteration_id)

    def _insert_iteration_and_register_coordinator(
        self,
        *,
        goal: Workflow,
        sequence_no: int,
        creation_reason: IterationCreationReason,
        iteration_goal: str,
    ) -> tuple[Iteration, IterationAttemptCoordinator]:
        iteration = self._iteration_store.insert(
            workflow_id=goal.id,
            sequence_no=sequence_no,
            creation_reason=creation_reason,
            goal=iteration_goal,
            attempt_budget=self._config.default_attempt_budget,
        )
        self._append_iteration_id(goal, iteration.id)
        coordinator = IterationAttemptCoordinator(
            iteration_id=iteration.id,
            iteration_store=self._iteration_store,
            attempt_store=self._attempt_store,
            on_iteration_closed=self.handle_iteration_closed,
            orchestrator_factory=self._orchestrator_factory,
            task_store=self._task_store,
        )
        self._iteration_coordinators.register(coordinator)
        return iteration, coordinator

    def _route_iteration_closure(self, report: IterationClosureReport) -> None:
        iteration = self._iteration_store.get(report.iteration_id)
        if iteration is None:
            raise TaskCenterInvariantViolation(f"Iteration {report.iteration_id!r} not found")
        try:
            outcome = report.outcome
            if isinstance(outcome, SuccessDeferred):
                (
                    next_iteration,
                    next_coordinator,
                ) = self.create_deferred_iteration_with_coordinator(previous_iteration=iteration)
                self._start_deferred_iteration(
                    next_iteration=next_iteration,
                    next_coordinator=next_coordinator,
                    previous_report=report,
                )
            elif isinstance(outcome, (TerminalSuccess, AttemptPlanFailed)):
                self.close_workflow(
                    workflow_id=iteration.workflow_id,
                    succeeded=isinstance(outcome, TerminalSuccess),
                    final_iteration_id=iteration.id,
                    final_attempt_id=report.final_attempt_id,
                )
            else:  # pragma: no cover
                raise TaskCenterInvariantViolation(f"Unknown ClosureOutcome: {outcome!r}")
        finally:
            self._iteration_coordinators.deregister(iteration.id)

    def _start_deferred_iteration(
        self,
        *,
        next_iteration: Iteration,
        next_coordinator: IterationAttemptCoordinator,
        previous_report: IterationClosureReport,
    ) -> None:
        if self._orchestrator_factory is None:
            return
        try:
            next_coordinator.create_initial_attempt()
        except Exception:
            logger.exception(
                "WorkflowLifecycle: continuation attempt creation failed",
                extra={"iteration_id": next_iteration.id},
            )
            latest_iteration = self._iteration_store.get(next_iteration.id)
            failed_attempt_id = (
                latest_iteration.latest_attempt_id if latest_iteration else None
            ) or previous_report.final_attempt_id
            self._iteration_store.set_status(
                next_iteration.id,
                status=IterationStatus.CANCELLED,
                closed_at=datetime.now(UTC),
            )
            self._iteration_coordinators.deregister(next_iteration.id)
            self.close_workflow(
                workflow_id=next_iteration.workflow_id,
                succeeded=False,
                final_iteration_id=next_iteration.id,
                final_attempt_id=failed_attempt_id,
            )


__all__ = [
    "WorkflowClosureCallback",
    "WorkflowLifecycle",
]
