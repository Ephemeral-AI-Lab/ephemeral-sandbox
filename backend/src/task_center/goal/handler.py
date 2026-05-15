"""Mission boundary — handler + factory + closure router + repository + ancestry.

Phase 7c absorbs ``mission/repository.py`` and ``mission/ancestry.py`` into
this single module.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.infra import (
    assert_continuation_episode_predecessor,
    assert_episode_id_unique_in_mission,
    assert_episode_sequence_contiguous,
    assert_mission_open,
)
from task_center._core.persistence import (
    AttemptStoreProtocol,
    EpisodeStoreProtocol,
    MissionStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.types import TaskCenterInvariantViolation, TaskCenterLifecycleConfig
from task_center.episode import EpisodeManager, EpisodeManagerRegistry, OrchestratorFactory
from task_center.episode.state import (
    AttemptPlanFailed,
    Episode,
    EpisodeClosureReport,
    EpisodeCreationReason,
    EpisodeStatus,
    SuccessContinue,
    TerminalSuccess,
)
from task_center.mission.state import Mission, MissionClosureReport, MissionStatus

logger = logging.getLogger(__name__)

MissionClosureReportSink = Callable[[MissionClosureReport], None]


# ---- Mission CRUD ----------------------------------------------------------


class MissionRepository:
    """CRUD + closure helpers for :class:`Mission` records."""

    def __init__(self, mission_store: MissionStoreProtocol) -> None:
        self._mission_store = mission_store

    def create(
        self, *, task_center_run_id: str, requested_by_task_id: str, goal: str,
    ) -> Mission:
        return self._mission_store.insert(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=requested_by_task_id,
            goal=goal,
        )

    def require(self, mission_id: str) -> Mission:
        mission = self._mission_store.get(mission_id)
        if mission is None:
            raise TaskCenterInvariantViolation(f"Mission {mission_id!r} not found")
        return mission

    def append_episode_id(self, mission: Mission, episode_id: str) -> Mission:
        assert_episode_id_unique_in_mission(mission, episode_id)
        return self._mission_store.append_episode_id(mission.id, episode_id)

    def close(
        self,
        *,
        mission_id: str,
        succeeded: bool,
        final_episode_id: str,
        final_attempt_id: str | None,
    ) -> tuple[Mission, MissionClosureReport]:
        """Close the mission and synthesise its :class:`MissionClosureReport`."""
        mission = self.require(mission_id)
        assert_mission_open(mission)
        report = MissionClosureReport(
            mission_id=mission_id,
            requested_by_task_id=mission.requested_by_task_id,
            outcome="success" if succeeded else "failed",
            final_episode_id=final_episode_id,
            final_attempt_id=final_attempt_id,
        )
        updated = self._mission_store.set_status(
            mission_id,
            status=MissionStatus.SUCCEEDED if succeeded else MissionStatus.FAILED,
            final_outcome=report.to_final_outcome(),
            closed_at=datetime.now(UTC),
        )
        return updated, report


# ---- Ancestry --------------------------------------------------------------


def nested_mission_depth(
    *,
    mission_id: str,
    mission_store: MissionStoreProtocol,
    episode_store: EpisodeStoreProtocol,
    attempt_store: AttemptStoreProtocol,
    task_store: TaskStoreProtocol,
) -> int:
    """Number of mission ancestors on the chain INCLUDING ``mission_id``."""
    depth = 0
    seen_mission_ids: set[str] = set()
    current_mission_id = mission_id
    while True:
        if current_mission_id in seen_mission_ids:
            raise TaskCenterInvariantViolation(
                "Cycle detected while resolving mission ancestry."
            )
        seen_mission_ids.add(current_mission_id)
        depth += 1
        current_mission = mission_store.get(current_mission_id)
        if current_mission is None:
            raise TaskCenterInvariantViolation(
                f"Mission {current_mission_id!r} was not found."
            )
        parent_task = task_store.get_task(current_mission.requested_by_task_id)
        if parent_task is None:
            return depth
        parent_attempt_id = str(parent_task.get("task_center_attempt_id") or "")
        if not parent_attempt_id:
            return depth
        parent_attempt = attempt_store.get(parent_attempt_id)
        if parent_attempt is None:
            raise TaskCenterInvariantViolation(
                f"Parent Attempt {parent_attempt_id!r} was not found."
            )
        parent_episode = episode_store.get(parent_attempt.episode_id)
        if parent_episode is None:
            raise TaskCenterInvariantViolation(
                f"Parent Episode {parent_attempt.episode_id!r} was not found."
            )
        current_mission_id = parent_episode.mission_id


class EpisodeFactory:
    """Creates :class:`Episode` rows + their :class:`EpisodeManager`."""

    def __init__(
        self,
        *,
        mission_repository: MissionRepository,
        episode_store: EpisodeStoreProtocol,
        attempt_store: AttemptStoreProtocol,
        manager_registry: EpisodeManagerRegistry,
        config: TaskCenterLifecycleConfig,
        on_episode_closed,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self._mission_repository = mission_repository
        self._episode_store = episode_store
        self._attempt_store = attempt_store
        self._manager_registry = manager_registry
        self._config = config
        self._on_episode_closed = on_episode_closed
        self._orchestrator_factory = orchestrator_factory
        self._task_store = task_store

    def create_initial(self, *, mission_id: str) -> tuple[Episode, EpisodeManager]:
        mission = self._mission_repository.require(mission_id)
        assert_mission_open(mission)
        assert_episode_sequence_contiguous(mission, new_sequence_no=1)
        return self._insert_and_spawn(
            mission=mission,
            sequence_no=1,
            creation_reason=EpisodeCreationReason.INITIAL,
            goal=mission.goal,
        )

    def create_continuation(
        self, *, previous_episode: Episode,
    ) -> tuple[Episode, EpisodeManager]:
        mission = self._mission_repository.require(previous_episode.mission_id)
        assert_mission_open(mission)
        assert_continuation_episode_predecessor(previous_episode)
        new_sequence_no = previous_episode.sequence_no + 1
        assert_episode_sequence_contiguous(mission, new_sequence_no=new_sequence_no)
        # predecessor invariant guarantees continuation_goal is not None.
        return self._insert_and_spawn(
            mission=mission,
            sequence_no=new_sequence_no,
            creation_reason=EpisodeCreationReason.PARTIAL_CONTINUATION,
            goal=previous_episode.continuation_goal,  # type: ignore[arg-type]
        )

    def _insert_and_spawn(
        self,
        *,
        mission: Mission,
        sequence_no: int,
        creation_reason: EpisodeCreationReason,
        goal: str,
    ) -> tuple[Episode, EpisodeManager]:
        episode = self._episode_store.insert(
            mission_id=mission.id,
            sequence_no=sequence_no,
            creation_reason=creation_reason,
            goal=goal,
            attempt_budget=self._config.default_attempt_budget,
        )
        self._mission_repository.append_episode_id(mission, episode.id)
        manager = EpisodeManager(
            episode_id=episode.id,
            episode_store=self._episode_store,
            attempt_store=self._attempt_store,
            on_episode_closed=self._on_episode_closed,
            orchestrator_factory=self._orchestrator_factory,
            task_store=self._task_store,
        )
        self._manager_registry.register(manager)
        return episode, manager


class EpisodeClosureRouter:
    """Routes :class:`EpisodeClosureReport` to continuation or mission close."""

    def __init__(
        self,
        *,
        factory: EpisodeFactory,
        episode_store: EpisodeStoreProtocol,
        manager_registry: EpisodeManagerRegistry,
        close_mission,
    ) -> None:
        self._factory = factory
        self._episode_store = episode_store
        self._manager_registry = manager_registry
        self._close_mission = close_mission

    def route(self, report: EpisodeClosureReport) -> None:
        episode = self._episode_store.get(report.episode_id)
        if episode is None:
            raise TaskCenterInvariantViolation(
                f"Episode {report.episode_id!r} not found"
            )
        try:
            outcome = report.outcome
            if isinstance(outcome, SuccessContinue):
                next_episode, next_manager = self._factory.create_continuation(
                    previous_episode=episode
                )
                self._start_continuation(
                    next_episode=next_episode,
                    next_manager=next_manager,
                    previous_report=report,
                )
            elif isinstance(outcome, (TerminalSuccess, AttemptPlanFailed)):
                self._close_mission(
                    mission_id=episode.mission_id,
                    succeeded=isinstance(outcome, TerminalSuccess),
                    final_episode_id=episode.id,
                    final_attempt_id=report.final_attempt_id,
                )
            else:  # pragma: no cover
                raise TaskCenterInvariantViolation(f"Unknown ClosureOutcome: {outcome!r}")
        finally:
            self._manager_registry.deregister(episode.id)

    def _start_continuation(
        self,
        *,
        next_episode,
        next_manager,
        previous_report: EpisodeClosureReport,
    ) -> None:
        if self._factory._orchestrator_factory is None:
            return
        try:
            next_manager.create_initial_attempt()
        except Exception:
            logger.exception(
                "EpisodeClosureRouter: continuation attempt creation failed",
                extra={"episode_id": next_episode.id},
            )
            latest_episode = self._episode_store.get(next_episode.id)
            failed_attempt_id = (
                (latest_episode.latest_attempt_id if latest_episode else None)
                or previous_report.final_attempt_id
            )
            self._episode_store.set_status(
                next_episode.id,
                status=EpisodeStatus.CANCELLED,
                closed_at=datetime.now(UTC),
            )
            self._manager_registry.deregister(next_episode.id)
            self._close_mission(
                mission_id=next_episode.mission_id,
                succeeded=False,
                final_episode_id=next_episode.id,
                final_attempt_id=failed_attempt_id,
            )


class MissionHandler:
    """Facade composing the mission repository, episode factory, and closure router."""

    def __init__(
        self,
        *,
        mission_store: MissionStoreProtocol,
        episode_store: EpisodeStoreProtocol,
        attempt_store: AttemptStoreProtocol,
        manager_registry: EpisodeManagerRegistry,
        config: TaskCenterLifecycleConfig,
        deliver_closure_report: MissionClosureReportSink | None = None,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self._deliver_closure_report = deliver_closure_report
        self._manager_registry = manager_registry
        self._repository = MissionRepository(mission_store)
        self._factory = EpisodeFactory(
            mission_repository=self._repository,
            episode_store=episode_store,
            attempt_store=attempt_store,
            manager_registry=manager_registry,
            config=config,
            on_episode_closed=self.handle_episode_closed,
            orchestrator_factory=orchestrator_factory,
            task_store=task_store,
        )
        self._router = EpisodeClosureRouter(
            factory=self._factory,
            episode_store=episode_store,
            manager_registry=manager_registry,
            close_mission=self.close_mission,
        )

    def create_mission(
        self, *, task_center_run_id: str, requested_by_task_id: str, goal: str,
    ) -> Mission:
        return self._repository.create(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=requested_by_task_id,
            goal=goal,
        )

    def create_initial_episode_with_manager(
        self, *, mission_id: str,
    ) -> tuple[Episode, EpisodeManager]:
        return self._factory.create_initial(mission_id=mission_id)

    def create_continuation_episode_with_manager(
        self, *, previous_episode: Episode,
    ) -> tuple[Episode, EpisodeManager]:
        return self._factory.create_continuation(previous_episode=previous_episode)

    def handle_episode_closed(self, report: EpisodeClosureReport) -> None:
        self._router.route(report)

    def close_mission(
        self,
        *,
        mission_id: str,
        succeeded: bool,
        final_episode_id: str,
        final_attempt_id: str | None,
    ) -> Mission:
        updated, report = self._repository.close(
            mission_id=mission_id,
            succeeded=succeeded,
            final_episode_id=final_episode_id,
            final_attempt_id=final_attempt_id,
        )
        if self._deliver_closure_report is not None:
            self._deliver_closure_report(report)
        return updated


__all__ = [
    "EpisodeClosureRouter",
    "EpisodeFactory",
    "MissionClosureReportSink",
    "MissionHandler",
    "MissionRepository",
    "nested_mission_depth",
]
