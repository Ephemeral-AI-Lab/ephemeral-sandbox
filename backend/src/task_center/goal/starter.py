"""GoalStarter — single safe path for executor → delegated goal start.

Owns parent-task CAS, deferred orchestrator startup, and compensation on
failure for ``submit_execution_handoff``.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from task_center.goal.close_report_router import (
    GoalClosureReportRouter,
)
from task_center.goal.handler import GoalHandler
from task_center.goal.state import (
    GoalClosureReport,
    Goal,
    GoalStatus,
)
from task_center._core.types import TaskCenterInvariantViolation
from task_center.trial.orchestrator import TrialOrchestrator
from task_center.iteration import OrchestratorFactory
from task_center.trial.state import TrialFailReason, TrialStatus
from task_center.trial.runtime import TrialDeps
from task_center.iteration.state import Iteration, IterationStatus
from task_center.task_state import TaskCenterTaskStatus

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class StartedGoal:
    parent_task_id: str
    parent_attempt_id: str | None  # None when caller is the top-level executor
    goal_id: str
    initial_iteration_id: str
    initial_trial_id: str
    goal: str


class GoalStarter:
    """Single orchestration entry point for executor → delegated goal start."""

    def __init__(
        self,
        *,
        runtime: TrialDeps,
        orchestrator_factory: OrchestratorFactory | None = None,
    ) -> None:
        self._runtime = runtime
        self._orchestrator_factory = orchestrator_factory or (
            lambda attempt, on_attempt_closed: TrialOrchestrator(
                attempt=attempt,
                on_attempt_closed=on_attempt_closed,
                runtime=self._runtime,
            )
        )

    def start(self, *, parent_task_id: str, goal: str) -> StartedGoal:
        parent_task = self._assert_parent_running_and_no_open_child(parent_task_id)
        task_center_run_id = str(parent_task.get("task_center_run_id") or "")
        if not task_center_run_id.strip():
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} has no run id."
            )
        parent_attempt_id = _parent_attempt_id(parent_task)

        handler = self._build_handler()
        created_goal = handler.create_goal(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
            goal=goal,
        )
        iteration, iteration_manager = handler.create_initial_iteration_with_manager(
            goal_id=created_goal.id,
        )

        initial_attempt = None
        try:
            initial_attempt = iteration_manager.create_unstarted_initial_attempt()
            self._mark_parent_waiting(
                parent_task_id=parent_task_id,
                parent_attempt_id=parent_attempt_id,
                goal=created_goal,
                iteration=iteration,
                attempt_id=initial_attempt.id,
                goal_str=goal,
            )
            iteration_manager.start_attempt(initial_attempt)
        except Exception:
            self._compensate_failed_start(
                goal=created_goal,
                iteration=iteration,
                initial_trial_id=initial_attempt.id if initial_attempt else None,
                parent_task_id=parent_task_id,
            )
            raise

        return StartedGoal(
            parent_task_id=parent_task_id,
            parent_attempt_id=parent_attempt_id,
            goal_id=created_goal.id,
            initial_iteration_id=iteration.id,
            initial_trial_id=initial_attempt.id,
            goal=goal,
        )

    def _build_handler(self) -> GoalHandler:
        manager_registry = self._runtime.manager_registry
        if manager_registry is None:
            raise TaskCenterInvariantViolation(
                "GoalStarter requires an iteration manager registry."
            )
        router = GoalClosureReportRouter(runtime=self._runtime)
        return GoalHandler(
            goal_store=self._runtime.goal_store,
            iteration_store=self._runtime.iteration_store,
            trial_store=self._runtime.trial_store,
            manager_registry=manager_registry,
            config=self._runtime.lifecycle_config,
            deliver_closure_report=router.deliver,
            orchestrator_factory=self._orchestrator_factory,
        )

    def _assert_parent_running_and_no_open_child(
        self, parent_task_id: str
    ) -> dict[str, Any]:
        task = self._runtime.task_store.get_task(parent_task_id)
        if task is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} was not found."
            )
        if task.get("status") != TaskCenterTaskStatus.RUNNING.value:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} is not running; "
                "delegated goal start requires a running generator task."
            )
        open_goals = [
            r
            for r in self._runtime.goal_store.list_for_executor_task(parent_task_id)
            if r.is_open
        ]
        if open_goals:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} already has an open "
                f"delegated goal {open_goals[0].id!r}."
            )
        return task

    def _mark_parent_waiting(
        self,
        *,
        parent_task_id: str,
        parent_attempt_id: str | None,
        goal: Goal,
        iteration: Iteration,
        attempt_id: str,
        goal_str: str,
    ) -> None:
        target = self._runtime.lifecycle_target_for(
            task_id=parent_task_id, attempt_id=parent_attempt_id
        )
        if target is None:
            raise TaskCenterInvariantViolation(
                f"No lifecycle target registered for TaskCenter task "
                f"{parent_task_id!r}; goal start cannot proceed."
            )
        target.mark_waiting_mission(
            delegated_mission_id=goal.id,
            delegated_episode_id=iteration.id,
            delegated_attempt_id=attempt_id,
            goal=goal_str,
        )

    def _compensate_failed_start(
        self,
        *,
        goal: Goal,
        iteration: Iteration,
        initial_trial_id: str | None,
        parent_task_id: str,
    ) -> None:
        """Best-effort rollback: trial -> iteration -> goal -> parent.

        Each step is independent; failures are logged via ``logger.exception``
        but never block subsequent steps. If parent restore fails we route a
        synthetic failed close-report so the parent does not stay orphaned in
        ``WAITING_MISSION``.
        """
        now = datetime.now(UTC)
        runtime = self._runtime

        def _do(step_name, action) -> bool:
            try:
                action()
                return True
            except Exception:
                logger.exception(
                    "GoalStart compensation step %r failed", step_name
                )
                return False

        _do("close_unstarted_trial", lambda: self._close_unstarted_trial(
            initial_trial_id, now=now
        ))
        _do("cancel_iteration", lambda: runtime.iteration_store.set_status(
            iteration.id, status=IterationStatus.CANCELLED, closed_at=now
        ))
        _do("cancel_goal", lambda: runtime.goal_store.set_status(
            goal.id, status=GoalStatus.CANCELLED,
            final_outcome=None, closed_at=now,
        ))
        if not _do("restore_parent", lambda: self._restore_parent(parent_task_id)):
            _do("synthetic_close_report", lambda: GoalClosureReportRouter(
                runtime=runtime
            ).deliver(GoalClosureReport(
                goal_id=goal.id,
                requested_by_task_id=parent_task_id,
                outcome="failed",
                final_iteration_id=iteration.id,
                final_trial_id=initial_trial_id,
            )))
        if runtime.manager_registry is not None:
            runtime.manager_registry.deregister(iteration.id)

    def _restore_parent(self, parent_task_id: str) -> None:
        parent_task = self._runtime.task_store.get_task(parent_task_id)
        attempt_id = _parent_attempt_id(parent_task) if parent_task else None
        target = self._runtime.lifecycle_target_for(
            task_id=parent_task_id, attempt_id=attempt_id
        )
        if target is not None:
            target.restore_running_after_failed_mission_start()
            return
        self._runtime.task_store.set_task_status_if_current(
            parent_task_id,
            expected_status=TaskCenterTaskStatus.WAITING_MISSION.value,
            status=TaskCenterTaskStatus.RUNNING.value,
        )

    def _close_unstarted_trial(
        self, trial_id: str | None, *, now: datetime
    ) -> None:
        if trial_id is None:
            return
        trial = self._runtime.trial_store.get(trial_id)
        if trial is None or trial.is_closed:
            return
        self._runtime.trial_store.close(
            trial_id,
            status=TrialStatus.FAILED,
            fail_reason=TrialFailReason.STARTUP_FAILED,
            closed_at=now,
        )


def _parent_attempt_id(task: dict[str, Any]) -> str | None:
    raw = str(task.get("task_center_attempt_id") or "")
    return raw if raw else None
