"""TaskCenter package primitives — invariant exception, task-id helpers, lifecycle config.

Persistence I/O Protocols live in :mod:`task_center._core.persistence`.
"""

from __future__ import annotations

from dataclasses import dataclass


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
    """Configurable knobs for the goal/iteration/attempt lifecycle.

    ``default_attempt_budget`` is applied to every Iteration created by
    ``WorkflowLifecycle`` unless overridden per-call.
    """

    default_attempt_budget: int = 2


__all__ = [
    "TaskCenterInvariantViolation",
    "TaskCenterLifecycleConfig",
    "evaluator_task_id",
    "generator_task_id",
    "planner_task_id",
]
