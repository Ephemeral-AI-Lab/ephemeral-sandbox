"""Goal lifecycle facade.

:class:`GoalHandler` is the single entry point external callers use to
create a delegated goal, drive its iteration chain, and close it. The
internal collaborators (:class:`GoalRepository`, :class:`IterationFactory`,
:class:`IterationClosureRouter`) live in :mod:`task_center.goal.collaborators`;
ancestry depth lives in :mod:`task_center.goal.ancestry`.
"""

from __future__ import annotations

from collections.abc import Callable

from task_center._core.persistence import (
    AttemptStoreProtocol,
    GoalStoreProtocol,
    IterationStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.primitives import TaskCenterLifecycleConfig
from task_center.goal.collaborators import (
    GoalRepository,
    IterationClosureRouter,
    IterationFactory,
)
from task_center.goal.state import Goal, GoalClosureReport
from task_center.iteration import IterationManager, IterationManagerRegistry, OrchestratorFactory
from task_center.iteration.state import Iteration, IterationClosureReport


GoalClosureReportSink = Callable[[GoalClosureReport], None]


class GoalHandler:
    """Facade composing the goal repository, iteration factory, and closure router."""

    def __init__(
        self,
        *,
        goal_store: GoalStoreProtocol,
        iteration_store: IterationStoreProtocol,
        attempt_store: AttemptStoreProtocol,
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
            attempt_store=attempt_store,
            manager_registry=manager_registry,
            config=config,
            on_iteration_closed=self.handle_iteration_closed,
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
        self,
        *,
        task_center_run_id: str,
        requested_by_task_id: str,
        goal: str,
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
        final_attempt_id: str | None,
    ) -> Goal:
        updated, report = self._repository.close(
            goal_id=goal_id,
            succeeded=succeeded,
            final_iteration_id=final_iteration_id,
            final_attempt_id=final_attempt_id,
        )
        if self._deliver_closure_report is not None:
            self._deliver_closure_report(report)
        return updated


__all__ = [
    "GoalClosureReportSink",
    "GoalHandler",
]
