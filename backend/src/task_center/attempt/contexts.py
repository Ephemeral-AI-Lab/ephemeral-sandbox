"""Role-narrow dependency contexts for TaskCenter lifecycle modules.

The original :class:`AttemptDeps` carries 8 stores + 2 registries + an
optional composer + an optional controller + an audit sink. Most call
sites use 2–4 fields, so the "service-locator-on-a-frozen-dataclass"
shape obscures what each method actually needs.

This module exposes narrow Protocol views that callers can declare as
their dependency type. The concrete :class:`AttemptDeps` instance
structurally satisfies all of them, so the wiring layer is unchanged.

Adopting the narrow contexts is **opt-in** — each lifecycle class can
migrate independently — and the win is that constructor signatures
document the real coupling. ``def __init__(self, *, stores:
TaskCenterStores)`` is honest about what the class actually touches.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from task_center.persistence import (
    AttemptStoreProtocol,
    EpisodeStoreProtocol,
    MissionStoreProtocol,
    TaskStoreProtocol,
)

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from task_center.attempt.orchestrator_registry import (
        AttemptOrchestratorRegistry,
    )
    from task_center.attempt.launcher import EphemeralAttemptAgentLauncher
    from task_center.attempt.state import Attempt
    from task_center.config import TaskCenterLifecycleConfig
    from task_center.context_engine.composer import ContextComposer
    from task_center.episode.registry import EpisodeManagerRegistry
    from task_center.lifecycle import LifecycleTarget


@dataclass(frozen=True, slots=True)
class TaskCenterStores:
    """The store quintet shared by every lifecycle class.

    Bundled as a single value so collaborators that touch all four stores
    (``MissionRepository``, ``EpisodeFactory``, ``EpisodeClosureRouter``)
    take one parameter instead of four. Concrete construction lives at
    the wiring layer (``TaskCenterEntryCoordinator`` /
    ``MissionStarter``); call sites that already hold an
    :class:`AttemptDeps` can derive a :class:`TaskCenterStores` via
    :meth:`AttemptDeps.stores`.
    """

    mission_store: MissionStoreProtocol
    episode_store: EpisodeStoreProtocol
    attempt_store: AttemptStoreProtocol
    task_store: TaskStoreProtocol


class AttemptStageCtx(Protocol):
    """Dependencies for any attempt-stage actor (planner + generator-DAG dispatch).

    Both planner-stage orchestration and generator-DAG dispatch read the
    attempt/episode/mission rows, write task + attempt rows, and dispatch
    launches through the agent launcher + composer. The original
    PlannerCtx + GeneratorCtx Protocols had identical surfaces (lever #9
    collapsed them).
    """

    mission_store: MissionStoreProtocol
    episode_store: EpisodeStoreProtocol
    attempt_store: AttemptStoreProtocol
    task_store: TaskStoreProtocol
    agent_launcher: EphemeralAttemptAgentLauncher
    orchestrator_registry: AttemptOrchestratorRegistry

    def run_id_for_attempt(self, attempt: Attempt) -> str: ...

    def require_composer(self) -> ContextComposer: ...


class EpisodeLifecycleCtx(Protocol):
    """Dependencies for :class:`EpisodeManager` and the episode-closure router."""

    mission_store: MissionStoreProtocol
    episode_store: EpisodeStoreProtocol
    attempt_store: AttemptStoreProtocol
    task_store: TaskStoreProtocol
    orchestrator_registry: AttemptOrchestratorRegistry
    manager_registry: EpisodeManagerRegistry | None
    lifecycle_config: TaskCenterLifecycleConfig


class MissionLifecycleCtx(EpisodeLifecycleCtx, Protocol):
    """Dependencies for :class:`MissionStarter`.

    Adds ``lifecycle_target_for`` to the episode-lifecycle slice
    (for entry-vs-attempt branching).
    """

    def lifecycle_target_for(
        self, *, task_id: str, attempt_id: str | None
    ) -> LifecycleTarget | None: ...


class LaunchCtx(Protocol):
    """Dependencies for :class:`LaunchBuilder` — composer access + stores."""

    mission_store: MissionStoreProtocol
    episode_store: EpisodeStoreProtocol

    def run_id_for_attempt(self, attempt: Attempt) -> str: ...

    def require_composer(self) -> ContextComposer: ...


__all__ = [
    "AttemptStageCtx",
    "EpisodeLifecycleCtx",
    "LaunchCtx",
    "MissionLifecycleCtx",
    "TaskCenterStores",
]
