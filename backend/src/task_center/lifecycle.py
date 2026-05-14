"""Polymorphic lifecycle target for parent tasks waiting on delegated missions.

A "parent task" is either:

1. The top-level entry executor (``task_center_attempt_id is None``), whose
   lifecycle is owned by :class:`EntryTaskController`.
2. A generator task inside an attempt (``task_center_attempt_id`` is set),
   whose lifecycle is jointly owned by :class:`AttemptOrchestrator` and the
   task row itself.

Without this seam, every call site that touches "the parent task waiting on
a mission" branches on ``attempt_id is None``. The :class:`LifecycleTarget`
protocol collapses those four branches (mission start, mission start
compensation, close-report delivery, run-exhaustion report) into a single
polymorphic dispatch.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from task_center.exceptions import TaskCenterInvariantViolation
from task_center.task.state import TaskCenterTaskStatus

if TYPE_CHECKING:
    from task_center.persistence import TaskStoreProtocol
    from task_center.attempt.orchestrator import AttemptOrchestrator
    from task_center.mission.state import MissionClosureReport


class LifecycleTarget(Protocol):
    """Lifecycle owner for one parent task.

    ``task_id`` is the parent task whose status the target manages.
    Implementations:

    - :class:`task_center.entry.controller.EntryTaskController` — entry mode.
    - :class:`GeneratorTaskLifecycle` — attempt mode (one per parent
      generator task that called the handoff).
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
    """:class:`LifecycleTarget` for a generator task inside an attempt.

    Wraps the attempt id, the task store, and a lazy reference to the
    :class:`AttemptOrchestrator` so the entry-vs-attempt branching sites
    collapse to one ``runtime.lifecycle_target_for(...).method(...)`` call.

    The orchestrator reference is lazy because ``mark_waiting_mission`` and
    ``restore_running_after_failed_mission_start`` only need the task store
    — they don't go through the orchestrator. Only
    ``apply_mission_closure_report`` requires the orchestrator and resolves
    it at call time, raising ``TaskCenterInvariantViolation`` if no
    orchestrator is registered (matching the original close-report
    router's contract).
    """

    task_id: str
    attempt_id: str
    task_store: TaskStoreProtocol
    orchestrator_lookup: Callable[[str], AttemptOrchestrator | None]

    def apply_mission_closure_report(
        self, report: MissionClosureReport
    ) -> None:
        orchestrator = self.orchestrator_lookup(self.attempt_id)
        if orchestrator is None:
            raise TaskCenterInvariantViolation(
                f"Parent AttemptOrchestrator for attempt "
                f"{self.attempt_id!r} is not registered; close-report "
                "delivery requires an active parent orchestrator."
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


__all__ = ["LifecycleTarget", "GeneratorTaskLifecycle"]
