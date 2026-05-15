"""TaskCenter public package surface.

External callers import lifecycle types, orchestrators, submissions, and
sandbox helpers from this package root::

    from task_center import (
        TrialOrchestrator,
        ContextScope,
        start_task_center_entry_run,
    )

Internal modules import from the canonical submodule path (e.g.
``task_center.goal.state`` for ``Goal``). The submodule paths are
stable; this package root is the convenience facade for outside-the-package
callers.

Public names are exposed via ``__getattr__`` so that importing a submodule
(``from task_center.goal.state import Goal``) does NOT trigger the
heavy agent-launch / context-engine load chain. The cycle would otherwise
be: db.stores → task_center root → agent_launch.composer → predicates →
goal.ancestry → db.stores. Lazy loading keeps the DTO submodules
import-cycle-safe.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from task_center.context_engine.core import ContextComposer, LaunchBundle
    from task_center._core.agent_routing import PredicateRegistry
    from task_center.trial.generator_dag import ordered_generator_tasks
    from task_center.trial.orchestrator import TrialOrchestrator
    from task_center.trial.runtime import TrialDeps
    from task_center.trial.state import (
        Trial,
        TrialFailReason,
        TrialStage,
        TrialStatus,
    )
    from task_center.context_engine.core import (
        AgentDefinitionValidationError,
    )
    from task_center.context_engine.packet import ContextPacket
    from task_center.context_engine.recipes_registry import RecipeRegistry
    from task_center.context_engine.scope import ContextScope
    from task_center.entry import EntryTaskController
    from task_center.entry.coordinator import start_task_center_entry_run
    from task_center.entry import TaskCenterSandboxBridge
    from task_center.iteration.state import (
        Iteration,
        IterationCreationReason,
        IterationStatus,
    )
    from task_center._core.types import TaskCenterInvariantViolation
    from task_center.goal.starter import GoalStarter, StartedGoal
    from task_center.goal.state import Goal, GoalStatus
    from task_center.task_state import (
        EvaluatorSubmission,
        GeneratorSubmission,
        PlannedGeneratorTask,
        PlannerSubmission,
    )


# Map: public name → (submodule, name_in_submodule)
_EXPORTS: dict[str, tuple[str, str]] = {
    "AgentDefinitionValidationError": (
        "task_center.context_engine.core",
        "AgentDefinitionValidationError",
    ),
    "Trial": ("task_center.trial.state", "Trial"),
    "TrialDeps": ("task_center.trial.runtime", "TrialDeps"),
    "TrialFailReason": ("task_center.trial.state", "TrialFailReason"),
    "TrialOrchestrator": (
        "task_center.trial.orchestrator",
        "TrialOrchestrator",
    ),
    "TrialStage": ("task_center.trial.state", "TrialStage"),
    "TrialStatus": ("task_center.trial.state", "TrialStatus"),
    "ContextComposer": ("task_center.context_engine.core", "ContextComposer"),
    "ContextPacket": ("task_center.context_engine.packet", "ContextPacket"),
    "ContextScope": ("task_center.context_engine.scope", "ContextScope"),
    "EntryTaskController": (
        "task_center.entry",
        "EntryTaskController",
    ),
    "Iteration": ("task_center.iteration.state", "Iteration"),
    "IterationCreationReason": (
        "task_center.iteration.state",
        "IterationCreationReason",
    ),
    "IterationStatus": ("task_center.iteration.state", "IterationStatus"),
    "EvaluatorSubmission": ("task_center.task_state", "EvaluatorSubmission"),
    "GeneratorSubmission": ("task_center.task_state", "GeneratorSubmission"),
    "LaunchBundle": ("task_center.context_engine.core", "LaunchBundle"),
    "Goal": ("task_center.goal.state", "Goal"),
    "GoalStarter": ("task_center.goal.starter", "GoalStarter"),
    "GoalStatus": ("task_center.goal.state", "GoalStatus"),
    "PlannedGeneratorTask": ("task_center.task_state", "PlannedGeneratorTask"),
    "PlannerSubmission": ("task_center.task_state", "PlannerSubmission"),
    "PredicateRegistry": (
        "task_center._core.agent_routing",
        "PredicateRegistry",
    ),
    "RecipeRegistry": (
        "task_center.context_engine.recipes_registry",
        "RecipeRegistry",
    ),
    "StartedGoal": ("task_center.goal.starter", "StartedGoal"),
    "TaskCenterInvariantViolation": (
        "task_center._core.types",
        "TaskCenterInvariantViolation",
    ),
    "TaskCenterSandboxBridge": (
        "task_center.entry",
        "TaskCenterSandboxBridge",
    ),
    "ordered_generator_tasks": (
        "task_center.trial.generator_dag",
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
