"""Collaborators composed by :class:`GoalHandler`.

Not re-exported by ``task_center.goal``'s facade — :class:`GoalHandler`
is the only consumer. Holds the three classes the handler composes:

* :class:`GoalRepository` — CRUD + closure helpers around the goal store.
* :class:`IterationFactory` — creates iteration rows + their managers.
* :class:`IterationClosureRouter` — routes :class:`IterationClosureReport`
  to either continuation or goal close.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.invariants import (
    assert_continuation_iteration_predecessor,
    assert_goal_open,
    assert_iteration_id_unique_in_goal,
    assert_iteration_sequence_contiguous,
)
from task_center._core.persistence import (
    AttemptStoreProtocol,
    GoalStoreProtocol,
    IterationStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.primitives import (
    TaskCenterInvariantViolation,
    TaskCenterLifecycleConfig,
)
from task_center.goal.state import Goal, GoalClosureReport, GoalStatus
from task_center.iteration import (
    IterationManager,
    IterationManagerRegistry,
    OrchestratorFactory,
)
from task_center.iteration.state import (
    AttemptPlanFailed,
    Iteration,
    IterationClosureReport,
    IterationCreationReason,
    IterationStatus,
    SuccessContinue,
    TerminalSuccess,
)

logger = logging.getLogger(__name__)


CloseGoalCallback = Callable[..., Goal]


class GoalRepository:
    """CRUD + closure helpers for :class:`Goal` records."""

    def __init__(self, goal_store: GoalStoreProtocol) -> None:
        self._goal_store = goal_store

    def create(
        self,
        *,
        task_center_run_id: str,
        requested_by_task_id: str,
        goal: str,
    ) -> Goal:
        return self._goal_store.insert(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=requested_by_task_id,
            goal=goal,
        )

    def require(self, goal_id: str) -> Goal:
        goal = self._goal_store.get(goal_id)
        if goal is None:
            raise TaskCenterInvariantViolation(f"Goal {goal_id!r} not found")
        return goal

    def append_iteration_id(self, goal: Goal, iteration_id: str) -> Goal:
        assert_iteration_id_unique_in_goal(goal, iteration_id)
        return self._goal_store.append_iteration_id(goal.id, iteration_id)

    def close(
        self,
        *,
        goal_id: str,
        succeeded: bool,
        final_iteration_id: str,
        final_attempt_id: str | None,
    ) -> tuple[Goal, GoalClosureReport]:
        """Close the goal and synthesise its :class:`GoalClosureReport`."""
        goal = self.require(goal_id)
        assert_goal_open(goal)
        report = GoalClosureReport(
            goal_id=goal_id,
            requested_by_task_id=goal.requested_by_task_id,
            outcome="success" if succeeded else "failed",
            final_iteration_id=final_iteration_id,
            final_attempt_id=final_attempt_id,
        )
        updated = self._goal_store.set_status(
            goal_id,
            status=GoalStatus.SUCCEEDED if succeeded else GoalStatus.FAILED,
            final_outcome=report.to_final_outcome(),
            closed_at=datetime.now(UTC),
        )
        return updated, report


class IterationFactory:
    """Creates :class:`Iteration` rows + their :class:`IterationManager`."""

    def __init__(
        self,
        *,
        goal_repository: GoalRepository,
        iteration_store: IterationStoreProtocol,
        attempt_store: AttemptStoreProtocol,
        manager_registry: IterationManagerRegistry,
        config: TaskCenterLifecycleConfig,
        on_iteration_closed: Callable[[IterationClosureReport], None],
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self._goal_repository = goal_repository
        self._iteration_store = iteration_store
        self._attempt_store = attempt_store
        self._manager_registry = manager_registry
        self._config = config
        self._on_iteration_closed = on_iteration_closed
        self._orchestrator_factory = orchestrator_factory
        self._task_store = task_store

    @property
    def has_orchestrator_factory(self) -> bool:
        return self._orchestrator_factory is not None

    def create_initial(self, *, goal_id: str) -> tuple[Iteration, IterationManager]:
        goal = self._goal_repository.require(goal_id)
        assert_goal_open(goal)
        assert_iteration_sequence_contiguous(goal, new_sequence_no=1)
        return self._insert_and_spawn(
            goal=goal,
            sequence_no=1,
            creation_reason=IterationCreationReason.INITIAL,
            iteration_goal=goal.goal,
        )

    def create_continuation(
        self, *, previous_iteration: Iteration,
    ) -> tuple[Iteration, IterationManager]:
        goal = self._goal_repository.require(previous_iteration.goal_id)
        assert_goal_open(goal)
        assert_continuation_iteration_predecessor(previous_iteration)
        new_sequence_no = previous_iteration.sequence_no + 1
        assert_iteration_sequence_contiguous(goal, new_sequence_no=new_sequence_no)
        # predecessor invariant guarantees continuation_goal is not None.
        return self._insert_and_spawn(
            goal=goal,
            sequence_no=new_sequence_no,
            creation_reason=IterationCreationReason.PARTIAL_CONTINUATION,
            iteration_goal=previous_iteration.continuation_goal,  # type: ignore[arg-type]
        )

    def _insert_and_spawn(
        self,
        *,
        goal: Goal,
        sequence_no: int,
        creation_reason: IterationCreationReason,
        iteration_goal: str,
    ) -> tuple[Iteration, IterationManager]:
        iteration = self._iteration_store.insert(
            goal_id=goal.id,
            sequence_no=sequence_no,
            creation_reason=creation_reason,
            goal=iteration_goal,
            attempt_budget=self._config.default_attempt_budget,
        )
        self._goal_repository.append_iteration_id(goal, iteration.id)
        manager = IterationManager(
            iteration_id=iteration.id,
            iteration_store=self._iteration_store,
            attempt_store=self._attempt_store,
            on_iteration_closed=self._on_iteration_closed,
            orchestrator_factory=self._orchestrator_factory,
            task_store=self._task_store,
        )
        self._manager_registry.register(manager)
        return iteration, manager


class IterationClosureRouter:
    """Routes :class:`IterationClosureReport` to continuation or goal close."""

    def __init__(
        self,
        *,
        factory: IterationFactory,
        iteration_store: IterationStoreProtocol,
        manager_registry: IterationManagerRegistry,
        close_goal: CloseGoalCallback,
    ) -> None:
        self._factory = factory
        self._iteration_store = iteration_store
        self._manager_registry = manager_registry
        self._close_goal = close_goal

    def route(self, report: IterationClosureReport) -> None:
        iteration = self._iteration_store.get(report.iteration_id)
        if iteration is None:
            raise TaskCenterInvariantViolation(
                f"Iteration {report.iteration_id!r} not found"
            )
        try:
            outcome = report.outcome
            if isinstance(outcome, SuccessContinue):
                next_iteration, next_manager = self._factory.create_continuation(
                    previous_iteration=iteration
                )
                self._start_continuation(
                    next_iteration=next_iteration,
                    next_manager=next_manager,
                    previous_report=report,
                )
            elif isinstance(outcome, (TerminalSuccess, AttemptPlanFailed)):
                self._close_goal(
                    goal_id=iteration.goal_id,
                    succeeded=isinstance(outcome, TerminalSuccess),
                    final_iteration_id=iteration.id,
                    final_attempt_id=report.final_attempt_id,
                )
            else:  # pragma: no cover
                raise TaskCenterInvariantViolation(
                    f"Unknown ClosureOutcome: {outcome!r}"
                )
        finally:
            self._manager_registry.deregister(iteration.id)

    def _start_continuation(
        self,
        *,
        next_iteration: Iteration,
        next_manager: IterationManager,
        previous_report: IterationClosureReport,
    ) -> None:
        if not self._factory.has_orchestrator_factory:
            return
        try:
            next_manager.create_initial_attempt()
        except Exception:
            logger.exception(
                "IterationClosureRouter: continuation attempt creation failed",
                extra={"iteration_id": next_iteration.id},
            )
            latest_iteration = self._iteration_store.get(next_iteration.id)
            failed_attempt_id = (
                (latest_iteration.latest_attempt_id if latest_iteration else None)
                or previous_report.final_attempt_id
            )
            self._iteration_store.set_status(
                next_iteration.id,
                status=IterationStatus.CANCELLED,
                closed_at=datetime.now(UTC),
            )
            self._manager_registry.deregister(next_iteration.id)
            self._close_goal(
                goal_id=next_iteration.goal_id,
                succeeded=False,
                final_iteration_id=next_iteration.id,
                final_attempt_id=failed_attempt_id,
            )


__all__ = [
    "GoalRepository",
    "IterationClosureRouter",
    "IterationFactory",
]
