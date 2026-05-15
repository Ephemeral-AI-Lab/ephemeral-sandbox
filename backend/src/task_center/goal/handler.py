"""Goal boundary — handler + factory + closure router + repository + ancestry.

Phase 7c absorbs ``mission/repository.py`` and ``mission/ancestry.py`` into
this single module.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.infra import (
    assert_continuation_iteration_predecessor,
    assert_iteration_id_unique_in_goal,
    assert_iteration_sequence_contiguous,
    assert_goal_open,
)
from task_center._core.persistence import (
    TrialStoreProtocol,
    IterationStoreProtocol,
    GoalStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.types import TaskCenterInvariantViolation, TaskCenterLifecycleConfig
from task_center.iteration import IterationManager, IterationManagerRegistry, OrchestratorFactory
from task_center.iteration.state import (
    TrialPlanFailed,
    Iteration,
    IterationClosureReport,
    IterationCreationReason,
    IterationStatus,
    SuccessContinue,
    TerminalSuccess,
)
from task_center.goal.state import Goal, GoalClosureReport, GoalStatus

logger = logging.getLogger(__name__)

GoalClosureReportSink = Callable[[GoalClosureReport], None]


# ---- Goal CRUD ----------------------------------------------------------


class GoalRepository:
    """CRUD + closure helpers for :class:`Goal` records."""

    def __init__(self, goal_store: GoalStoreProtocol) -> None:
        self._goal_store = goal_store

    def create(
        self, *, task_center_run_id: str, requested_by_task_id: str, goal: str,
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
        final_trial_id: str | None,
    ) -> tuple[Goal, GoalClosureReport]:
        """Close the goal and synthesise its :class:`GoalClosureReport`."""
        goal = self.require(goal_id)
        assert_goal_open(goal)
        report = GoalClosureReport(
            goal_id=goal_id,
            requested_by_task_id=goal.requested_by_task_id,
            outcome="success" if succeeded else "failed",
            final_iteration_id=final_iteration_id,
            final_trial_id=final_trial_id,
        )
        updated = self._goal_store.set_status(
            goal_id,
            status=GoalStatus.SUCCEEDED if succeeded else GoalStatus.FAILED,
            final_outcome=report.to_final_outcome(),
            closed_at=datetime.now(UTC),
        )
        return updated, report


# ---- Ancestry --------------------------------------------------------------


def nested_goal_depth(
    *,
    goal_id: str,
    goal_store: GoalStoreProtocol,
    iteration_store: IterationStoreProtocol,
    trial_store: TrialStoreProtocol,
    task_store: TaskStoreProtocol,
) -> int:
    """Number of goal ancestors on the chain INCLUDING ``goal_id``."""
    depth = 0
    seen_goal_ids: set[str] = set()
    current_goal_id = goal_id
    while True:
        if current_goal_id in seen_goal_ids:
            raise TaskCenterInvariantViolation(
                "Cycle detected while resolving goal ancestry."
            )
        seen_goal_ids.add(current_goal_id)
        depth += 1
        current_goal = goal_store.get(current_goal_id)
        if current_goal is None:
            raise TaskCenterInvariantViolation(
                f"Goal {current_goal_id!r} was not found."
            )
        parent_task = task_store.get_task(current_goal.requested_by_task_id)
        if parent_task is None:
            return depth
        parent_attempt_id = str(parent_task.get("task_center_attempt_id") or "")
        if not parent_attempt_id:
            return depth
        parent_attempt = trial_store.get(parent_attempt_id)
        if parent_attempt is None:
            raise TaskCenterInvariantViolation(
                f"Parent Trial {parent_attempt_id!r} was not found."
            )
        parent_iteration = iteration_store.get(parent_attempt.iteration_id)
        if parent_iteration is None:
            raise TaskCenterInvariantViolation(
                f"Parent Iteration {parent_attempt.iteration_id!r} was not found."
            )
        current_goal_id = parent_iteration.goal_id


class IterationFactory:
    """Creates :class:`Iteration` rows + their :class:`IterationManager`."""

    def __init__(
        self,
        *,
        goal_repository: GoalRepository,
        iteration_store: IterationStoreProtocol,
        trial_store: TrialStoreProtocol,
        manager_registry: IterationManagerRegistry,
        config: TaskCenterLifecycleConfig,
        on_episode_closed,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self._goal_repository = goal_repository
        self._iteration_store = iteration_store
        self._trial_store = trial_store
        self._manager_registry = manager_registry
        self._config = config
        self._on_episode_closed = on_episode_closed
        self._orchestrator_factory = orchestrator_factory
        self._task_store = task_store

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
            trial_budget=self._config.default_attempt_budget,
        )
        self._goal_repository.append_iteration_id(goal, iteration.id)
        manager = IterationManager(
            iteration_id=iteration.id,
            iteration_store=self._iteration_store,
            trial_store=self._trial_store,
            on_episode_closed=self._on_episode_closed,
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
        close_goal,
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
            elif isinstance(outcome, (TerminalSuccess, TrialPlanFailed)):
                self._close_goal(
                    goal_id=iteration.goal_id,
                    succeeded=isinstance(outcome, TerminalSuccess),
                    final_iteration_id=iteration.id,
                    final_trial_id=report.final_trial_id,
                )
            else:  # pragma: no cover
                raise TaskCenterInvariantViolation(f"Unknown ClosureOutcome: {outcome!r}")
        finally:
            self._manager_registry.deregister(iteration.id)

    def _start_continuation(
        self,
        *,
        next_iteration,
        next_manager,
        previous_report: IterationClosureReport,
    ) -> None:
        if self._factory._orchestrator_factory is None:
            return
        try:
            next_manager.create_initial_attempt()
        except Exception:
            logger.exception(
                "IterationClosureRouter: continuation trial creation failed",
                extra={"iteration_id": next_iteration.id},
            )
            latest_iteration = self._iteration_store.get(next_iteration.id)
            failed_trial_id = (
                (latest_iteration.latest_trial_id if latest_iteration else None)
                or previous_report.final_trial_id
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
                final_trial_id=failed_trial_id,
            )


class GoalHandler:
    """Facade composing the goal repository, iteration factory, and closure router."""

    def __init__(
        self,
        *,
        goal_store: GoalStoreProtocol,
        iteration_store: IterationStoreProtocol,
        trial_store: TrialStoreProtocol,
        manager_registry: IterationManagerRegistry,
        config: TaskCenterLifecycleConfig,
        deliver_closure_report: GoalClosureReportSink | None = None,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self._deliver_closure_report = deliver_closure_report
        self._manager_registry = manager_registry
        self._repository = GoalRepository(goal_store)
        self._factory = IterationFactory(
            goal_repository=self._repository,
            iteration_store=iteration_store,
            trial_store=trial_store,
            manager_registry=manager_registry,
            config=config,
            on_episode_closed=self.handle_iteration_closed,
            orchestrator_factory=orchestrator_factory,
            task_store=task_store,
        )
        self._router = IterationClosureRouter(
            factory=self._factory,
            iteration_store=iteration_store,
            manager_registry=manager_registry,
            close_goal=self.close_goal,
        )

    def create_goal(
        self, *, task_center_run_id: str, requested_by_task_id: str, goal: str,
    ) -> Goal:
        return self._repository.create(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=requested_by_task_id,
            goal=goal,
        )

    def create_initial_iteration_with_manager(
        self, *, goal_id: str,
    ) -> tuple[Iteration, IterationManager]:
        return self._factory.create_initial(goal_id=goal_id)

    def create_continuation_iteration_with_manager(
        self, *, previous_iteration: Iteration,
    ) -> tuple[Iteration, IterationManager]:
        return self._factory.create_continuation(previous_iteration=previous_iteration)

    def handle_iteration_closed(self, report: IterationClosureReport) -> None:
        self._router.route(report)

    def close_goal(
        self,
        *,
        goal_id: str,
        succeeded: bool,
        final_iteration_id: str,
        final_trial_id: str | None,
    ) -> Goal:
        updated, report = self._repository.close(
            goal_id=goal_id,
            succeeded=succeeded,
            final_iteration_id=final_iteration_id,
            final_trial_id=final_trial_id,
        )
        if self._deliver_closure_report is not None:
            self._deliver_closure_report(report)
        return updated


__all__ = [
    "IterationClosureRouter",
    "IterationFactory",
    "GoalClosureReportSink",
    "GoalHandler",
    "GoalRepository",
    "nested_goal_depth",
]
