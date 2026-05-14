"""Runtime + lifecycle dependency seams for harness attempt orchestration.

Phase 7e merger: bundles the former ``attempt/lifecycle.py``
(``LifecycleTarget`` protocol + ``GeneratorTaskLifecycle``) into this module
so the runtime DI surface and the polymorphic parent-task lifecycle owner
sit side-by-side.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Protocol

from audit.base import AuditSink, NoopAuditSink

from task_center.attempt.state import Attempt
from task_center._core.types import TaskCenterLifecycleConfig
from task_center.episode.registry import EpisodeManagerRegistry
from task_center._core.types import TaskCenterInvariantViolation
from task_center._core.persistence import (
    AttemptStoreProtocol,
    EpisodeStoreProtocol,
    MissionStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.types import RegisteredAttemptOrchestrator
from task_center.task_state import TaskCenterTaskRole, TaskCenterTaskStatus

if TYPE_CHECKING:
    from task_center.attempt.launch import EphemeralAttemptAgentLauncher
    from task_center.attempt.orchestrator_registry import (
        AttemptOrchestratorRegistry,
    )
    from task_center.context_engine.composer import ContextComposer
    from task_center.attempt.contexts import TaskCenterStores
    from task_center.entry.controller import EntryTaskController
    from task_center.mission.state import MissionClosureReport


@dataclass(frozen=True, slots=True)
class AgentLaunch:
    task_id: str
    task_center_run_id: str
    attempt_id: str | None
    role: TaskCenterTaskRole
    agent_name: str
    rendered_prompt: str
    needs: tuple[str, ...]
    context_packet_id: str | None = None
    mission_id: str | None = None
    # Per-launch extension bag. Use for knobs the launcher or runtime can
    # opt into (priority, latency budget, retry policy) without forcing a
    # new field + four call-site edits per knob. Keys are caller-defined;
    # consumers should ``metadata.get(...)`` defensively.
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class AttemptDeps:
    mission_store: MissionStoreProtocol
    episode_store: EpisodeStoreProtocol
    attempt_store: AttemptStoreProtocol
    task_store: TaskStoreProtocol
    agent_launcher: EphemeralAttemptAgentLauncher
    orchestrator_registry: AttemptOrchestratorRegistry
    manager_registry: EpisodeManagerRegistry | None = None
    lifecycle_config: TaskCenterLifecycleConfig = field(default_factory=TaskCenterLifecycleConfig)
    # When set, orchestrator + dispatcher route launches through the composer
    # to obtain a rendered rendered_prompt + selected agent definition.
    # Optional so existing tests can continue without composer wiring.
    composer: ContextComposer | None = None
    # Lifecycle controller for the top-level entry executor. ``None`` for
    # delegated-only runtimes.
    # The close-report router and launcher use this to dispatch lifecycle
    # events for entry tasks whose ``task_center_attempt_id`` is None.
    entry_task_controller: EntryTaskController | None = None
    audit_sink: AuditSink = field(default_factory=NoopAuditSink)

    @property
    def stores(self) -> TaskCenterStores:
        """Narrow view of the store quintet for collaborators that touch
        only persistence.

        See :mod:`task_center.contexts` for the broader role-narrow
        Protocol palette (:class:`AttemptStageCtx`,
        :class:`EpisodeLifecycleCtx`, :class:`MissionLifecycleCtx`,
        :class:`LaunchCtx`).
        """
        # Local import keeps the runtime module free of an eager
        # contexts dependency; the protocols reference back to AttemptDeps
        # only for documentation.
        from task_center.attempt.contexts import TaskCenterStores

        return TaskCenterStores(
            mission_store=self.mission_store,
            episode_store=self.episode_store,
            attempt_store=self.attempt_store,
            task_store=self.task_store,
        )

    def run_id_for_attempt(self, attempt: Attempt) -> str:
        episode = self.episode_store.get(attempt.episode_id)
        if episode is None:
            raise TaskCenterInvariantViolation(
                f"Episode {attempt.episode_id!r} not found for "
                f"Attempt {attempt.id!r}"
            )
        mission = self.mission_store.get(episode.mission_id)
        if mission is None:
            raise TaskCenterInvariantViolation(
                f"Mission {episode.mission_id!r} not "
                f"found for Episode {episode.id!r}"
            )
        return mission.task_center_run_id

    def require_composer(self) -> ContextComposer:
        if self.composer is None:
            raise TaskCenterInvariantViolation(
                "AttemptDeps requires a ContextComposer for harness "
                "agent launches; none was wired."
            )
        return self.composer

    def entry_task_controller_for(
        self, task_id: str
    ) -> EntryTaskController | None:
        """Return the entry controller iff it's bound to *task_id*.

        Used at the four entry-mode dispatch sites (mission starter
        parent-waiting + compensation + duplicate-child check, close-report
        router, submission resolver) so each site collapses to one call
        instead of duplicating the ``is not None and task_id == X`` guard.
        Returns ``None`` for attempt-mode tasks or when no controller is
        wired.
        """
        controller = self.entry_task_controller
        if controller is None or controller.task_id != task_id:
            return None
        return controller

    def lifecycle_target_for(
        self, *, task_id: str, attempt_id: str | None
    ) -> LifecycleTarget | None:
        """Return the :class:`LifecycleTarget` for one parent task.

        For entry-mode (``attempt_id is None``), returns the
        :class:`EntryTaskController` bound to *task_id* if any. For
        attempt-mode, wraps the active orchestrator in a
        :class:`GeneratorTaskLifecycle`. Returns ``None`` when no target is
        registered — callers decide whether that's a hard error.
        """
        if attempt_id is None:
            return self.entry_task_controller_for(task_id)
        return GeneratorTaskLifecycle(
            task_id=task_id,
            attempt_id=attempt_id,
            task_store=self.task_store,
            orchestrator_lookup=self.orchestrator_registry.get,
        )


# ---- LifecycleTarget seam (polymorphic parent-task owner) ------------------


class LifecycleTarget(Protocol):
    """Lifecycle owner for one parent task waiting on a delegated mission.

    Implementations: :class:`EntryTaskController` (entry mode), and
    :class:`GeneratorTaskLifecycle` (attempt mode).
    """

    task_id: str

    def apply_mission_closure_report(
        self, report: MissionClosureReport
    ) -> None: ...

    def mark_waiting_mission(
        self,
        *,
        delegated_mission_id: str,
        delegated_episode_id: str,
        delegated_attempt_id: str,
        goal: str,
    ) -> None: ...

    def restore_running_after_failed_mission_start(self) -> None: ...


@dataclass(frozen=True, slots=True)
class GeneratorTaskLifecycle:
    """:class:`LifecycleTarget` for a generator task inside an attempt."""

    task_id: str
    attempt_id: str
    task_store: TaskStoreProtocol
    orchestrator_lookup: Callable[[str], RegisteredAttemptOrchestrator | None]

    def apply_mission_closure_report(
        self, report: MissionClosureReport
    ) -> None:
        orchestrator = self.orchestrator_lookup(self.attempt_id)
        if orchestrator is None:
            raise TaskCenterInvariantViolation(
                f"Parent AttemptOrchestrator for attempt {self.attempt_id!r} is "
                "not registered; close-report delivery requires an active "
                "parent orchestrator."
            )
        orchestrator.apply_mission_closure_report(report)

    def mark_waiting_mission(
        self,
        *,
        delegated_mission_id: str,
        delegated_episode_id: str,
        delegated_attempt_id: str,
        goal: str,
    ) -> None:
        summary = {
            "outcome": "mission_start",
            "summary": "Waiting on delegated mission solution.",
            "payload": {
                "mission_id": delegated_mission_id,
                "initial_episode_id": delegated_episode_id,
                "initial_attempt_id": delegated_attempt_id,
                "parent_attempt_id": self.attempt_id,
                "goal": goal,
            },
        }
        updated = self.task_store.set_task_status_if_current(
            self.task_id,
            expected_status=TaskCenterTaskStatus.RUNNING.value,
            status=TaskCenterTaskStatus.WAITING_MISSION.value,
            summary=summary,
        )
        if updated is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {self.task_id!r} was not running when the "
                "delegated mission start tried to mark it waiting."
            )

    def restore_running_after_failed_mission_start(self) -> None:
        self.task_store.set_task_status_if_current(
            self.task_id,
            expected_status=TaskCenterTaskStatus.WAITING_MISSION.value,
            status=TaskCenterTaskStatus.RUNNING.value,
        )
