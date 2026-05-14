"""MissionStarter — use-case boundary for delegated mission start.

Composes the mission, episode, manager, and parent-task owners into
the single safe mission-start path used by ``submit_execution_handoff``. Owns
parent-task CAS, deferred orchestrator startup, and compensation on failure.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from task_center.mission.close_report_delivery import (
    MissionClosureReportRouter,
)
from task_center.mission.handler import MissionHandler
from task_center.mission.state import (
    MissionClosureReport,
    Mission,
    MissionStatus,
)
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.state import AttemptFailReason, AttemptStatus
from task_center.attempt.runtime import AttemptDeps
from task_center.episode.state import Episode, EpisodeStatus
from task_center.task.state import TaskCenterTaskStatus

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class StartedMission:
    parent_task_id: str
    # ``None`` when the caller is the top-level entry executor.
    parent_attempt_id: str | None
    mission_id: str
    initial_episode_id: str
    initial_attempt_id: str
    goal: str


class MissionStarter:
    """Single orchestration entry point for executor → delegated mission start."""

    def __init__(self, *, runtime: AttemptDeps) -> None:
        self._runtime = runtime

    def start(
        self,
        *,
        parent_task_id: str,
        goal: str,
    ) -> StartedMission:
        parent_task = self._assert_parent_running_and_no_open_child(
            parent_task_id=parent_task_id,
        )
        task_center_run_id = str(parent_task.get("task_center_run_id") or "")
        if not task_center_run_id.strip():
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} has no run id."
            )
        parent_attempt_id = _parent_attempt_id(parent_task)

        handler = self._build_handler()
        delegated_mission = handler.create_mission(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
            goal=goal,
        )
        (
            initial_episode,
            episode_manager,
        ) = handler.create_initial_episode_with_manager(
            mission_id=delegated_mission.id,
        )

        initial_attempt = None
        try:
            initial_attempt = episode_manager.create_unstarted_initial_attempt()
            self._mark_parent_waiting(
                parent_task_id=parent_task_id,
                parent_task=parent_task,
                mission=delegated_mission,
                episode=initial_episode,
                attempt_id=initial_attempt.id,
                goal=goal,
            )
            episode_manager.start_attempt(initial_attempt)
        except Exception:
            self._compensate_failed_start(
                mission=delegated_mission,
                episode=initial_episode,
                initial_attempt_id=(
                    initial_attempt.id if initial_attempt is not None else None
                ),
                parent_task_id=parent_task_id,
            )
            raise

        # Narrowed by the ``try`` block above: the only path to here assigns
        # ``initial_attempt`` before ``start_attempt`` runs. The explicit
        # check makes the invariant self-defending under ``python -O``.
        if initial_attempt is None:
            raise TaskCenterInvariantViolation(
                "MissionStarter.start completed without assigning initial_attempt."
            )
        return StartedMission(
            parent_task_id=parent_task_id,
            parent_attempt_id=parent_attempt_id,
            mission_id=delegated_mission.id,
            initial_episode_id=initial_episode.id,
            initial_attempt_id=initial_attempt.id,
            goal=goal,
        )

    # ---- internal -------------------------------------------------------

    def _build_handler(self) -> MissionHandler:
        manager_registry = self._runtime.manager_registry
        if manager_registry is None:
            raise TaskCenterInvariantViolation(
                "MissionStarter requires an episode manager registry."
            )
        router = MissionClosureReportRouter(runtime=self._runtime)

        def _deliver(report: MissionClosureReport) -> None:
            router.deliver(report)

        return MissionHandler(
            mission_store=self._runtime.mission_store,
            episode_store=self._runtime.episode_store,
            attempt_store=self._runtime.attempt_store,
            manager_registry=manager_registry,
            config=self._runtime.lifecycle_config,
            deliver_closure_report=_deliver,
            orchestrator_factory=lambda attempt, on_attempt_closed: AttemptOrchestrator(
                attempt=attempt,
                on_attempt_closed=on_attempt_closed,
                runtime=self._runtime,
            ),
        )

    def _assert_parent_running_and_no_open_child(
        self,
        *,
        parent_task_id: str,
    ) -> dict[str, Any]:
        task = self._runtime.task_store.get_task(parent_task_id)
        if task is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} was not found."
            )
        if task.get("status") != TaskCenterTaskStatus.RUNNING.value:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} is not running; "
                "delegated mission start requires a running generator task."
            )
        existing_open = [
            r
            for r in self._runtime.mission_store.list_for_executor_task(
                parent_task_id
            )
            if r.is_open
        ]
        if existing_open:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} already has an open "
                f"delegated mission {existing_open[0].id!r}."
            )
        return task

    def _mark_parent_waiting(
        self,
        *,
        parent_task_id: str,
        parent_task: dict[str, Any],
        mission: Mission,
        episode: Episode,
        attempt_id: str,
        goal: str,
    ) -> None:
        target = self._runtime.lifecycle_target_for(
            task_id=parent_task_id,
            attempt_id=_parent_attempt_id(parent_task),
        )
        if target is None:
            raise TaskCenterInvariantViolation(
                f"No lifecycle target registered for TaskCenter task "
                f"{parent_task_id!r}; mission start cannot proceed."
            )
        target.mark_waiting_mission(
            delegated_mission_id=mission.id,
            delegated_episode_id=episode.id,
            delegated_attempt_id=attempt_id,
            goal=goal,
        )

    def _compensate_failed_start(
        self,
        *,
        mission: Mission,
        episode: Episode,
        initial_attempt_id: str | None,
        parent_task_id: str,
    ) -> None:
        """Best-effort rollback. Order: attempt → episode → mission → parent."""
        now = datetime.now(UTC)
        self._close_unstarted_attempt_after_failed_start(initial_attempt_id, now=now)
        try:
            self._runtime.episode_store.set_status(
                episode.id,
                status=EpisodeStatus.CANCELLED,
                closed_at=now,
            )
        except Exception:
            logger.exception(
                "MissionStarter: cancel episode failed",
            )
        try:
            self._runtime.mission_store.set_status(
                mission.id,
                status=MissionStatus.CANCELLED,
                final_outcome=None,
                closed_at=now,
            )
        except Exception:
            logger.exception(
                "MissionStarter: cancel mission failed",
            )
        try:
            parent_task = self._runtime.task_store.get_task(parent_task_id)
            attempt_id = (
                _parent_attempt_id(parent_task) if parent_task else None
            )
            target = self._runtime.lifecycle_target_for(
                task_id=parent_task_id, attempt_id=attempt_id
            )
            if target is not None:
                target.restore_running_after_failed_mission_start()
            else:
                self._runtime.task_store.set_task_status_if_current(
                    parent_task_id,
                    expected_status=TaskCenterTaskStatus.WAITING_MISSION.value,
                    status=TaskCenterTaskStatus.RUNNING.value,
                )
        except Exception:
            logger.critical(
                "MissionStarter: parent status rollback failed; "
                "task %r remains in WAITING_MISSION — attempting "
                "synthetic close-report recovery",
                parent_task_id,
                exc_info=True,
            )
            self._deliver_synthetic_failure_closure_report(
                mission=mission,
                episode=episode,
                initial_attempt_id=initial_attempt_id,
                parent_task_id=parent_task_id,
            )
        manager_registry = self._runtime.manager_registry
        if manager_registry is not None:
            manager_registry.deregister(episode.id)

    def _deliver_synthetic_failure_closure_report(
        self,
        *,
        mission: Mission,
        episode: Episode,
        initial_attempt_id: str | None,
        parent_task_id: str,
    ) -> None:
        """Last-resort recovery when direct rollback to RUNNING fails.

        Without this, a parent task can be orphaned in
        ``WAITING_MISSION`` with no automated driver to reset it.
        Routing a synthetic ``MissionClosureReport(outcome="failed")`` re-uses
        the close-report router so the controller / orchestrator unsticks the
        parent the same way it would for a normal failed-mission close. The
        router no-ops cleanly when the task already reached a terminal state.
        """
        try:
            router = MissionClosureReportRouter(runtime=self._runtime)
            router.deliver(
                MissionClosureReport(
                    mission_id=mission.id,
                    requested_by_task_id=parent_task_id,
                    outcome="failed",
                    final_episode_id=episode.id,
                    final_attempt_id=initial_attempt_id,
                )
            )
        except Exception:
            logger.critical(
                "MissionStarter: synthetic close-report delivery also failed; "
                "task %r remains in WAITING_MISSION and requires manual "
                "recovery",
                parent_task_id,
                exc_info=True,
            )

    def _close_unstarted_attempt_after_failed_start(
        self, attempt_id: str | None, *, now: datetime
    ) -> None:
        if attempt_id is None:
            return
        try:
            attempt = self._runtime.attempt_store.get(attempt_id)
            if attempt is None or attempt.is_closed:
                return
            self._runtime.attempt_store.close(
                attempt_id,
                status=AttemptStatus.FAILED,
                fail_reason=AttemptFailReason.STARTUP_FAILED,
                closed_at=now,
            )
        except Exception:
            logger.exception(
                "MissionStarter: failed to close attempt "
                "after mission-start failure",
            )


def _parent_attempt_id(task: dict[str, Any]) -> str | None:
    raw = str(task.get("task_center_attempt_id") or "")
    return raw if raw else None
