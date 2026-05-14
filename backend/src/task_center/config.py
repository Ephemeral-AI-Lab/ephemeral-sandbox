"""Runtime configuration for the TaskCenter lifecycle."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class TaskCenterLifecycleConfig:
    """Configurable knobs for the mission/episode/attempt lifecycle.

    ``default_attempt_budget`` is applied to every Episode created by
    ``MissionHandler`` unless overridden per-call.

    ``max_handoff_depth`` is the maximum nested-mission depth at which an
    executor profile still offers a handoff terminal. Above this, the
    leaf-executor variant is selected (success + failure terminals only).
    Range-named predicates read this through the config so renaming or
    re-tuning the threshold does not require touching any frontmatter.
    """

    default_attempt_budget: int = 2
    max_handoff_depth: int = 2
