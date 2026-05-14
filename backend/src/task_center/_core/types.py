"""TaskCenter package primitives — exceptions, ids, config, runtime Protocols.

Phase 7a bundle: collapses former `_core/exceptions.py`, `_core/ids.py`,
`_core/config.py`, and `_core/protocols.py` into a single primitive types
module. Persistence I/O Protocols stay in `_core/persistence.py` per the
iter4 plan amendment.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from task_center.mission.state import MissionClosureReport


# ---- Exceptions ------------------------------------------------------------


class TaskCenterInvariantViolation(Exception):
    """Raised when a harness lifecycle invariant is violated.

    Hard, non-tolerable harness state breach.
    """


# ---- Stable task ids -------------------------------------------------------


def planner_task_id(attempt_id: str) -> str:
    return f"{attempt_id}:planner"


def generator_task_id(attempt_id: str, local_task_id: str) -> str:
    return f"{attempt_id}:gen:{local_task_id}"


def evaluator_task_id(attempt_id: str) -> str:
    return f"{attempt_id}:evaluator"


# ---- Runtime configuration -------------------------------------------------


@dataclass(frozen=True, slots=True)
class TaskCenterLifecycleConfig:
    """Configurable knobs for the mission/episode/attempt lifecycle.

    ``default_attempt_budget`` is applied to every Episode created by
    ``MissionHandler`` unless overridden per-call. ``max_handoff_depth``
    is the maximum nested-mission depth at which an executor profile
    still offers a handoff terminal.
    """

    default_attempt_budget: int = 2
    max_handoff_depth: int = 2


# ---- Runtime Protocols (cycle-safe collaboration seams) --------------------


class RegisteredAttemptOrchestrator(Protocol):
    """The slice of :class:`AttemptOrchestrator` observed by collaborators."""

    @property
    def attempt_id(self) -> str: ...

    def start(self) -> None: ...

    def apply_mission_closure_report(
        self, report: MissionClosureReport
    ) -> None: ...


__all__ = [
    "RegisteredAttemptOrchestrator",
    "TaskCenterInvariantViolation",
    "TaskCenterLifecycleConfig",
    "evaluator_task_id",
    "generator_task_id",
    "planner_task_id",
]
