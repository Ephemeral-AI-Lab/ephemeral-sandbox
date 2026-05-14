"""Runtime configuration for the harness lifecycle."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class TaskCenterLifecycleConfig:
    """Configurable knobs for the mission/episode/attempt lifecycle.

    ``default_attempt_budget`` is applied to every Episode created by
    ``MissionHandler`` unless overridden per-call.
    """

    default_attempt_budget: int = 2
