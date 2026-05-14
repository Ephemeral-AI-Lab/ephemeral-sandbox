"""MissionHandler — mission boundary lifecycle service.

Only creator of ``Mission`` and ``Episode`` records, and the
spawner of ``EpisodeManager`` instances. Routes ``EpisodeClosureReport``
into either continuation episode creation or mission closure.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import UTC, datetime
from typing import Literal

from db.stores.mission_store import MissionStore
from db.stores.attempt_store import AttemptStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.episode_store import EpisodeStore
from task_center.mission.validation import (
    assert_continuation_episode_predecessor,
    assert_mission_open,
    assert_episode_id_unique_in_mission,
    assert_episode_sequence_contiguous,
)
from task_center.mission.mission import (
    MissionCloseReport,
    Mission,
    MissionStatus,
)
from task_center.config import TaskCenterLifecycleConfig
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.episode.closure_report import (
    AttemptPlanFailed,
    SuccessContinue,
    EpisodeClosureReport,
    TerminalSuccess,
)
from task_center.episode.manager import OrchestratorFactory, EpisodeManager
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.episode.episode import (
    Episode,
    EpisodeCreationReason,
    EpisodeStatus,
)


CloseReportSink = Callable[[MissionCloseReport], None]


class MissionHandler:
    """Owns the mission boundary: mission + episode creation, mission closure."""

    def __init__(
        self,
        *,
        mission_store: MissionStore,
        episode_store: EpisodeStore,
        attempt_store: AttemptStore,
        manager_registry: EpisodeManagerRegistry,
        config: TaskCenterLifecycleConfig,
        deliver_close_report: CloseReportSink | None = None,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskCenterStore | None = None,
    ) -> None:
        self._mission_store = mission_store
        self._episode_store = episode_store
        self._attempt_store = attempt_store
        self._manager_registry = manager_registry
        self._config = config
        self._deliver_close_report = deliver_close_report
        self._orchestrator_factory = orchestrator_factory
        self._task_store = task_store

    # ---- public API -----------------------------------------------------

    def create_mission(
        self,
        *,
        task_center_run_id: str,
        requested_by_task_id: str,
        goal: str,
    ) -> Mission:
        return self._mission_store.insert(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=requested_by_task_id,
            goal=goal,
        )

    def create_initial_episode_with_manager(
        self, *, mission_id: str
    ) -> tuple[Episode, EpisodeManager]:
        mission = self._require_mission(mission_id)
        assert_mission_open(mission)
        assert_episode_sequence_contiguous(mission, new_sequence_no=1)
        episode = self._episode_store.insert(
            mission_id=mission_id,
            sequence_no=1,
            creation_reason=EpisodeCreationReason.INITIAL,
            goal=mission.goal,
            attempt_budget=self._config.default_attempt_budget,
        )
        self._append_episode_to_mission(mission, episode)
        manager = self._spawn_episode_manager(episode)
        return episode, manager

    def create_continuation_episode_with_manager(
        self, *, previous_episode: Episode
    ) -> tuple[Episode, EpisodeManager]:
        mission = self._require_mission(previous_episode.mission_id)
        assert_mission_open(mission)
        assert_continuation_episode_predecessor(previous_episode)
        new_sequence_no = previous_episode.sequence_no + 1
        assert_episode_sequence_contiguous(mission, new_sequence_no=new_sequence_no)
        # Narrowed by ``assert_continuation_episode_predecessor`` above; the
        # explicit check makes the invariant self-defending under ``python -O``
        # where ``assert`` would be stripped.
        if previous_episode.continuation_goal is None:
            raise TaskCenterInvariantViolation(
                f"Previous episode {previous_episode.id!r} has no "
                "continuation_goal despite passing the predecessor invariant."
            )
        episode = self._episode_store.insert(
            mission_id=mission.id,
            sequence_no=new_sequence_no,
            creation_reason=EpisodeCreationReason.PARTIAL_CONTINUATION,
            goal=previous_episode.continuation_goal,
            attempt_budget=self._config.default_attempt_budget,
        )
        self._append_episode_to_mission(mission, episode)
        manager = self._spawn_episode_manager(episode)
        return episode, manager

    def handle_episode_closed(
        self, report: EpisodeClosureReport
    ) -> None:
        episode = self._episode_store.get(report.episode_id)
        if episode is None:
            raise TaskCenterInvariantViolation(
                f"Episode {report.episode_id!r} not found"
            )
        try:
            outcome = report.outcome
            if isinstance(outcome, SuccessContinue):
                (
                    next_episode,
                    next_manager,
                ) = self.create_continuation_episode_with_manager(
                    previous_episode=episode,
                )
                self._start_continuation_episode(
                    next_episode=next_episode,
                    next_manager=next_manager,
                    previous_report=report,
                )
            elif isinstance(outcome, TerminalSuccess):
                self.close_mission(
                    mission_id=episode.mission_id,
                    succeeded=True,
                    final_episode_id=episode.id,
                    final_attempt_id=report.final_attempt_id,
                )
            elif isinstance(outcome, AttemptPlanFailed):
                self.close_mission(
                    mission_id=episode.mission_id,
                    succeeded=False,
                    final_episode_id=episode.id,
                    final_attempt_id=report.final_attempt_id,
                )
            else:  # pragma: no cover - exhaustive over discriminated union
                raise TaskCenterInvariantViolation(
                    f"Unknown ClosureOutcome: {outcome!r}"
                )
        finally:
            self._manager_registry.deregister(episode.id)

    def close_mission(
        self,
        *,
        mission_id: str,
        succeeded: bool,
        final_episode_id: str,
        final_attempt_id: str | None,
    ) -> Mission:
        mission = self._require_mission(mission_id)
        assert_mission_open(mission)
        outcome_label: Literal["success", "failed"] = (
            "success" if succeeded else "failed"
        )
        close_report = MissionCloseReport(
            mission_id=mission_id,
            requested_by_task_id=mission.requested_by_task_id,
            outcome=outcome_label,
            final_episode_id=final_episode_id,
            final_attempt_id=final_attempt_id,
        )
        status = (
            MissionStatus.SUCCEEDED
            if succeeded
            else MissionStatus.FAILED
        )
        updated = self._mission_store.set_status(
            mission_id,
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
        next_episode: Episode,
        next_manager: EpisodeManager,
        previous_report: EpisodeClosureReport,
    ) -> None:
        """Create and start the continuation episode's initial attempt.

        Skipped when no ``orchestrator_factory`` is configured: in that case
        the test or harness driver is responsible for creating and stopping the
        attempt manually. Production paths always attach a factory through the
        mission starter, so continuation startup runs end-to-end.

        On startup failure the continuation episode is cancelled and the
        mission is closed as failed. If attempt insertion already happened, the
        close report points at that failed continuation attempt.
        """
        if self._orchestrator_factory is None:
            return
        try:
            next_manager.create_initial_attempt()
        except Exception:
            failed_attempt_id = self._latest_attempt_id_for_episode(
                next_episode.id
            ) or previous_report.final_attempt_id
            self._episode_store.set_status(
                next_episode.id,
                status=EpisodeStatus.CANCELLED,
                closed_at=datetime.now(UTC),
            )
            self._manager_registry.deregister(next_episode.id)
            self.close_mission(
                mission_id=next_episode.mission_id,
                succeeded=False,
                final_episode_id=next_episode.id,
                final_attempt_id=failed_attempt_id,
            )

    def _require_mission(self, mission_id: str) -> Mission:
        mission = self._mission_store.get(mission_id)
        if mission is None:
            raise TaskCenterInvariantViolation(
                f"Mission {mission_id!r} not found"
            )
        return mission

    def _append_episode_to_mission(
        self, mission: Mission, episode: Episode
    ) -> None:
        assert_episode_id_unique_in_mission(mission, episode.id)
        self._mission_store.append_episode_id(mission.id, episode.id)

    def _latest_attempt_id_for_episode(self, episode_id: str) -> str | None:
        episode = self._episode_store.get(episode_id)
        if episode is None:
            return None
        return episode.latest_attempt_id

    def _spawn_episode_manager(self, episode: Episode) -> EpisodeManager:
        manager = EpisodeManager(
            episode_id=episode.id,
            episode_store=self._episode_store,
            attempt_store=self._attempt_store,
            on_episode_closed=self.handle_episode_closed,
            orchestrator_factory=self._orchestrator_factory,
            task_store=self._task_store,
        )
        self._manager_registry.register(manager)
        return manager
