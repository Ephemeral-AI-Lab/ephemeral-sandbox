"""MissionStarter — single safe path for executor → delegated mission start.

Owns parent-task CAS, deferred orchestrator startup, and compensation on
failure for ``submit_execution_handoff``.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from task_center.mission.close_report_router import (
    MissionClosureReportRouter,
)
from task_center.mission.handler import MissionHandler
from task_center.mission.state import (
    MissionClosureReport,
    Mission,
    MissionStatus,
)
from task_center._core.types import TaskCenterInvariantViolation
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.episode import OrchestratorFactory
from task_center.attempt.state import AttemptFailReason, AttemptStatus
from task_center.attempt.runtime import AttemptDeps
from task_center.episode.state import Episode, EpisodeStatus
from task_center.task_state import TaskCenterTaskStatus

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class StartedMission:
    parent_task_id: str
    parent_attempt_id: str | None  # None when caller is the top-level executor
    mission_id: str
    initial_episode_id: str
    initial_attempt_id: str
    goal: str


class MissionStarter:
    """Single orchestration entry point for executor → delegated mission start."""

    def __init__(
        self,
        *,
        runtime: AttemptDeps,
        orchestrator_factory: OrchestratorFactory | None = None,
    ) -> None:
        self._runtime = runtime
        self._orchestrator_factory = orchestrator_factory or (
            lambda attempt, on_attempt_closed: AttemptOrchestrator(
                attempt=attempt,
                on_attempt_closed=on_attempt_closed,
                runtime=self._runtime,
            )
        )

    def start(self, *, parent_task_id: str, goal: str) -> StartedMission:
        parent_task = self._assert_parent_running_and_no_open_child(parent_task_id)
        task_center_run_id = str(parent_task.get("task_center_run_id") or "")
        if not task_center_run_id.strip():
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} has no run id."
            )
        parent_attempt_id = _parent_attempt_id(parent_task)

        handler = self._build_handler()
        mission = handler.create_mission(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
            goal=goal,
        )
        episode, episode_manager = handler.create_initial_episode_with_manager(
            mission_id=mission.id,
        )

        initial_attempt = None
        try:
            initial_attempt = episode_manager.create_unstarted_initial_attempt()
            self._mark_parent_waiting(
                parent_task_id=parent_task_id,
                parent_attempt_id=parent_attempt_id,
                mission=mission,
                episode=episode,
                attempt_id=initial_attempt.id,
                goal=goal,
            )
            episode_manager.start_attempt(initial_attempt)
        except Exception:
            self._compensate_failed_start(
                mission=mission,
                episode=episode,
                initial_attempt_id=initial_attempt.id if initial_attempt else None,
                parent_task_id=parent_task_id,
            )
            raise

        return StartedMission(
            parent_task_id=parent_task_id,
            parent_attempt_id=parent_attempt_id,
            mission_id=mission.id,
            initial_episode_id=episode.id,
            initial_attempt_id=initial_attempt.id,
            goal=goal,
        )

    def _build_handler(self) -> MissionHandler:
        manager_registry = self._runtime.manager_registry
        if manager_registry is None:
            raise TaskCenterInvariantViolation(
                "MissionStarter requires an episode manager registry."
            )
        router = MissionClosureReportRouter(runtime=self._runtime)
        return MissionHandler(
            mission_store=self._runtime.mission_store,
            episode_store=self._runtime.episode_store,
            attempt_store=self._runtime.attempt_store,
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
                "delegated mission start requires a running generator task."
            )
        open_missions = [
            r
            for r in self._runtime.mission_store.list_for_executor_task(parent_task_id)
            if r.is_open
        ]
        if open_missions:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} already has an open "
                f"delegated mission {open_missions[0].id!r}."
            )
        return task

    def _mark_parent_waiting(
        self,
        *,
        parent_task_id: str,
        parent_attempt_id: str | None,
        mission: Mission,
        episode: Episode,
        attempt_id: str,
        goal: str,
    ) -> None:
        target = self._runtime.lifecycle_target_for(
            task_id=parent_task_id, attempt_id=parent_attempt_id
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
        """Best-effort rollback: attempt -> episode -> mission -> parent.

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
                    "MissionStart compensation step %r failed", step_name
                )
                return False

        _do("close_unstarted_attempt", lambda: self._close_unstarted_attempt(
            initial_attempt_id, now=now
        ))
        _do("cancel_episode", lambda: runtime.episode_store.set_status(
            episode.id, status=EpisodeStatus.CANCELLED, closed_at=now
        ))
        _do("cancel_mission", lambda: runtime.mission_store.set_status(
            mission.id, status=MissionStatus.CANCELLED,
            final_outcome=None, closed_at=now,
        ))
        if not _do("restore_parent", lambda: self._restore_parent(parent_task_id)):
            _do("synthetic_close_report", lambda: MissionClosureReportRouter(
                runtime=runtime
            ).deliver(MissionClosureReport(
                mission_id=mission.id,
                requested_by_task_id=parent_task_id,
                outcome="failed",
                final_episode_id=episode.id,
                final_attempt_id=initial_attempt_id,
            )))
        if runtime.manager_registry is not None:
            runtime.manager_registry.deregister(episode.id)

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

    def _close_unstarted_attempt(
        self, attempt_id: str | None, *, now: datetime
    ) -> None:
        if attempt_id is None:
            return
        attempt = self._runtime.attempt_store.get(attempt_id)
        if attempt is None or attempt.is_closed:
            return
        self._runtime.attempt_store.close(
            attempt_id,
            status=AttemptStatus.FAILED,
            fail_reason=AttemptFailReason.STARTUP_FAILED,
            closed_at=now,
        )


def _parent_attempt_id(task: dict[str, Any]) -> str | None:
    raw = str(task.get("task_center_attempt_id") or "")
    return raw if raw else None
