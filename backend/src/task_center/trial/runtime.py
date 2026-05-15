"""Runtime + lifecycle dependency seams for harness trial orchestration.

Phase 7e merger: bundles the former ``attempt/lifecycle.py``
(``LifecycleTarget`` protocol + ``GeneratorTaskLifecycle``) into this module
so the runtime DI surface and the polymorphic parent-task lifecycle owner
sit side-by-side.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Protocol

from audit.base import AuditSink, NoopAuditSink

from task_center.trial.state import Trial
from task_center._core.types import TaskCenterLifecycleConfig
from task_center.iteration import IterationManagerRegistry
from task_center._core.types import TaskCenterInvariantViolation
from task_center._core.persistence import (
    TrialStoreProtocol,
    IterationStoreProtocol,
    GoalStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.types import RegisteredTrialOrchestrator
from task_center.task_state import TaskCenterTaskRole, TaskCenterTaskStatus

if TYPE_CHECKING:
    from task_center.trial.launch import EphemeralTrialAgentLauncher
    from task_center.trial.orchestrator_registry import (
        TrialOrchestratorRegistry,
    )
    from task_center.context_engine.core import ContextComposer
    from task_center.entry import EntryTaskController
    from task_center.goal.state import GoalClosureReport


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
    goal_id: str | None = None


@dataclass(frozen=True, slots=True)
class TrialDeps:
    goal_store: GoalStoreProtocol
    iteration_store: IterationStoreProtocol
    trial_store: TrialStoreProtocol
    task_store: TaskStoreProtocol
    agent_launcher: EphemeralTrialAgentLauncher
    orchestrator_registry: TrialOrchestratorRegistry
    manager_registry: IterationManagerRegistry | None = None
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

    def run_id_for_attempt(self, attempt: Trial) -> str:
        iteration = self.iteration_store.get(attempt.iteration_id)
        if iteration is None:
            raise TaskCenterInvariantViolation(
                f"Iteration {attempt.iteration_id!r} not found for "
                f"Trial {attempt.id!r}"
            )
        goal = self.goal_store.get(iteration.goal_id)
        if goal is None:
            raise TaskCenterInvariantViolation(
                f"Goal {iteration.goal_id!r} not "
                f"found for Iteration {iteration.id!r}"
            )
        return goal.task_center_run_id

    def require_composer(self) -> ContextComposer:
        if self.composer is None:
            raise TaskCenterInvariantViolation(
                "TrialDeps requires a ContextComposer for harness "
                "agent launches; none was wired."
            )
        return self.composer

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
            controller = self.entry_task_controller
            if controller is None or controller.task_id != task_id:
                return None
            return controller
        return GeneratorTaskLifecycle(
            task_id=task_id,
            attempt_id=attempt_id,
            task_store=self.task_store,
            orchestrator_lookup=self.orchestrator_registry.get,
        )


# ---- LifecycleTarget seam (polymorphic parent-task owner) ------------------


class LifecycleTarget(Protocol):
    """Lifecycle owner for one parent task waiting on a delegated goal.

    Implementations: :class:`EntryTaskController` (entry mode), and
    :class:`GeneratorTaskLifecycle` (attempt mode).
    """

    task_id: str

    def apply_goal_closure_report(
        self, report: GoalClosureReport
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
    """:class:`LifecycleTarget` for a generator task inside a trial."""

    task_id: str
    attempt_id: str
    task_store: TaskStoreProtocol
    orchestrator_lookup: Callable[[str], RegisteredTrialOrchestrator | None]

    def apply_goal_closure_report(
        self, report: GoalClosureReport
    ) -> None:
        orchestrator = self.orchestrator_lookup(self.attempt_id)
        if orchestrator is None:
            raise TaskCenterInvariantViolation(
                f"Parent TrialOrchestrator for trial {self.attempt_id!r} is "
                "not registered; close-report delivery requires an active "
                "parent orchestrator."
            )
        orchestrator.apply_goal_closure_report(report)

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
                "goal_id": delegated_mission_id,
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
