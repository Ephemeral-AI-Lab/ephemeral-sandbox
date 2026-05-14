"""Public TaskCenter API surface for callers outside ``task_center``."""

from __future__ import annotations

from typing import TYPE_CHECKING

from task_center.agent_launch.composer import ContextComposer, LaunchBundle
from task_center.agent_launch.predicates import PredicateRegistry
from task_center.attempt.generator_dag import ordered_generator_tasks
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.runtime import AttemptDeps
from task_center.attempt.state import Attempt
from task_center.context_engine.errors import AgentDefinitionValidationError
from task_center.context_engine.scope import ContextScope
from task_center.context_engine.recipes_registry import RecipeRegistry
from task_center.entry.controller import EntryTaskController
from task_center.entry.sandbox_bridge import TaskCenterSandboxBridge
from task_center.episode.episode import Episode
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.mission.mission import Mission
from task_center.mission.starter import MissionStarter, StartedMission
from task_center.task.models import (
    EvaluatorSubmission,
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannerSubmission,
)

if TYPE_CHECKING:
    from task_center.entry.coordinator import start_task_center_entry_run

def __getattr__(name: str) -> object:
    if name == "start_task_center_entry_run":
        from task_center.entry.coordinator import start_task_center_entry_run

        value = start_task_center_entry_run
        globals()[name] = value
        return value
    raise AttributeError(name)


__all__ = [
    "AgentDefinitionValidationError",
    "Attempt",
    "AttemptOrchestrator",
    "AttemptDeps",
    "ContextComposer",
    "ContextScope",
    "EntryTaskController",
    "Episode",
    "EvaluatorSubmission",
    "GeneratorSubmission",
    "LaunchBundle",
    "Mission",
    "MissionStarter",
    "PlannedGeneratorTask",
    "PlannerSubmission",
    "PredicateRegistry",
    "RecipeRegistry",
    "StartedMission",
    "TaskCenterInvariantViolation",
    "TaskCenterSandboxBridge",
    "ordered_generator_tasks",
    "start_task_center_entry_run",
]
