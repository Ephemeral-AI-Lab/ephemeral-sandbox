"""TaskCenter public package surface.

External callers import lifecycle types, orchestrators, submissions, and
sandbox helpers from this package root::

    from task_center import (
        AttemptOrchestrator,
        ContextScope,
        start_task_center_entry_run,
    )

Internal modules import from the canonical submodule path (e.g.
``task_center.mission.state`` for ``Mission``). The submodule paths are
stable; this package root is the convenience facade for outside-the-package
callers.

Public names are exposed via ``__getattr__`` so that importing a submodule
(``from task_center.mission.state import Mission``) does NOT trigger the
heavy agent-launch / context-engine load chain. The cycle would otherwise
be: db.stores → task_center root → agent_launch.composer → predicates →
mission.ancestry → db.stores. Lazy loading keeps the DTO submodules
import-cycle-safe.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from task_center.context_engine.composer import ContextComposer, LaunchBundle
    from task_center.agent_routing.predicates import PredicateRegistry
    from task_center.attempt.generator_dag import ordered_generator_tasks
    from task_center.attempt.orchestrator import AttemptOrchestrator
    from task_center.attempt.runtime import AttemptDeps
    from task_center.attempt.state import (
        Attempt,
        AttemptFailReason,
        AttemptStage,
        AttemptStatus,
    )
    from task_center.context_engine.errors import (
        AgentDefinitionValidationError,
    )
    from task_center.context_engine.packet import ContextPacket
    from task_center.context_engine.recipes_registry import RecipeRegistry
    from task_center.context_engine.scope import ContextScope
    from task_center.entry.controller import EntryTaskController
    from task_center.entry.coordinator import start_task_center_entry_run
    from task_center.entry.sandbox_bridge import TaskCenterSandboxBridge
    from task_center.episode.state import (
        Episode,
        EpisodeCreationReason,
        EpisodeStatus,
    )
    from task_center._core.types import TaskCenterInvariantViolation
    from task_center.mission.starter import MissionStarter, StartedMission
    from task_center.mission.state import Mission, MissionStatus
    from task_center.task_state import (
        EvaluatorSubmission,
        GeneratorSubmission,
        PlannedGeneratorTask,
        PlannerSubmission,
    )


# Map: public name → (submodule, name_in_submodule)
_EXPORTS: dict[str, tuple[str, str]] = {
    "AgentDefinitionValidationError": (
        "task_center.context_engine.errors",
        "AgentDefinitionValidationError",
    ),
    "Attempt": ("task_center.attempt.state", "Attempt"),
    "AttemptDeps": ("task_center.attempt.runtime", "AttemptDeps"),
    "AttemptFailReason": ("task_center.attempt.state", "AttemptFailReason"),
    "AttemptOrchestrator": (
        "task_center.attempt.orchestrator",
        "AttemptOrchestrator",
    ),
    "AttemptStage": ("task_center.attempt.state", "AttemptStage"),
    "AttemptStatus": ("task_center.attempt.state", "AttemptStatus"),
    "ContextComposer": ("task_center.context_engine.composer", "ContextComposer"),
    "ContextPacket": ("task_center.context_engine.packet", "ContextPacket"),
    "ContextScope": ("task_center.context_engine.scope", "ContextScope"),
    "EntryTaskController": (
        "task_center.entry.controller",
        "EntryTaskController",
    ),
    "Episode": ("task_center.episode.state", "Episode"),
    "EpisodeCreationReason": (
        "task_center.episode.state",
        "EpisodeCreationReason",
    ),
    "EpisodeStatus": ("task_center.episode.state", "EpisodeStatus"),
    "EvaluatorSubmission": ("task_center.task_state", "EvaluatorSubmission"),
    "GeneratorSubmission": ("task_center.task_state", "GeneratorSubmission"),
    "LaunchBundle": ("task_center.context_engine.composer", "LaunchBundle"),
    "Mission": ("task_center.mission.state", "Mission"),
    "MissionStarter": ("task_center.mission.starter", "MissionStarter"),
    "MissionStatus": ("task_center.mission.state", "MissionStatus"),
    "PlannedGeneratorTask": ("task_center.task_state", "PlannedGeneratorTask"),
    "PlannerSubmission": ("task_center.task_state", "PlannerSubmission"),
    "PredicateRegistry": (
        "task_center.agent_routing.predicates",
        "PredicateRegistry",
    ),
    "RecipeRegistry": (
        "task_center.context_engine.recipes_registry",
        "RecipeRegistry",
    ),
    "StartedMission": ("task_center.mission.starter", "StartedMission"),
    "TaskCenterInvariantViolation": (
        "task_center._core.types",
        "TaskCenterInvariantViolation",
    ),
    "TaskCenterSandboxBridge": (
        "task_center.entry.sandbox_bridge",
        "TaskCenterSandboxBridge",
    ),
    "ordered_generator_tasks": (
        "task_center.attempt.generator_dag",
        "ordered_generator_tasks",
    ),
    "start_task_center_entry_run": (
        "task_center.entry.coordinator",
        "start_task_center_entry_run",
    ),
}


def __getattr__(name: str) -> object:
    target = _EXPORTS.get(name)
    if target is None:
        raise AttributeError(f"module 'task_center' has no attribute {name!r}")
    module_path, attr = target
    import importlib

    module = importlib.import_module(module_path)
    value = getattr(module, attr)
    globals()[name] = value
    return value


__all__ = sorted(_EXPORTS)
