"""TaskCenter public package surface.

External callers import lifecycle types, orchestrators, submissions, and
sandbox helpers from this package root::

    from task_center import (
        AttemptOrchestrator,
        ContextScope,
        start_task_center_run,
    )

Internal modules import from the canonical submodule path (e.g.
``task_center.workflow.state`` for ``Workflow``). The package root is the stable
convenience facade for outside-the-package
callers.

Public names are exposed via ``__getattr__`` so that importing a submodule
(``from task_center.workflow.state import Workflow``) does NOT trigger the
heavy agent-launch / context-engine load chain. The cycle would otherwise
be: db.stores -> task_center root -> agent_launch.composer ->
terminal_tool_routing -> goal.ancestry -> db.stores. Lazy loading keeps the
DTO submodules import-cycle-safe.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from task_center.agent_launch.composer import AgentEntryComposer
    from task_center.agent_launch.entry_messages import AgentEntryMessages
    from task_center.attempt.generator_dag import ordered_generator_tasks
    from task_center.attempt.orchestrator import AttemptOrchestrator
    from task_center.attempt.deps import AttemptDeps
    from task_center.attempt.state import (
        Attempt,
        AttemptFailReason,
        AttemptStage,
        AttemptStatus,
    )
    from task_center.context_engine.core import (
        AgentDefinitionValidationError,
    )
    from task_center.context_engine.packet import ContextPacket
    from task_center.context_engine.recipes_registry import RecipeRegistry
    from task_center.context_engine.scope import ContextScope
    from task_center.entry.bootstrap import (
        TaskCenterEntry,
        TaskCenterEntryHandle,
        start_task_center_run,
    )
    from task_center.entry import TaskCenterSandboxProvisioner
    from task_center.iteration.state import (
        Iteration,
        IterationCreationReason,
        IterationStatus,
    )
    from task_center._core.primitives import TaskCenterInvariantViolation
    from task_center.workflow.starter import WorkflowStarter, StartedWorkflow
    from task_center.workflow.state import Workflow, WorkflowOrigin, WorkflowOriginKind, WorkflowStatus
    from task_center.submissions import (
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
    "Attempt": ("task_center.attempt.state", "Attempt"),
    "AttemptDeps": ("task_center.attempt.deps", "AttemptDeps"),
    "AttemptFailReason": ("task_center.attempt.state", "AttemptFailReason"),
    "AttemptOrchestrator": (
        "task_center.attempt.orchestrator",
        "AttemptOrchestrator",
    ),
    "AttemptStage": ("task_center.attempt.state", "AttemptStage"),
    "AttemptStatus": ("task_center.attempt.state", "AttemptStatus"),
    "AgentEntryComposer": (
        "task_center.agent_launch.composer",
        "AgentEntryComposer",
    ),
    "AgentEntryMessages": (
        "task_center.agent_launch.entry_messages",
        "AgentEntryMessages",
    ),
    "ContextPacket": ("task_center.context_engine.packet", "ContextPacket"),
    "ContextScope": ("task_center.context_engine.scope", "ContextScope"),
    "Iteration": ("task_center.iteration.state", "Iteration"),
    "IterationCreationReason": (
        "task_center.iteration.state",
        "IterationCreationReason",
    ),
    "IterationStatus": ("task_center.iteration.state", "IterationStatus"),
    "EvaluatorSubmission": ("task_center.submissions", "EvaluatorSubmission"),
    "GeneratorSubmission": ("task_center.submissions", "GeneratorSubmission"),
    "Workflow": ("task_center.workflow.state", "Workflow"),
    "WorkflowOrigin": ("task_center.workflow.state", "WorkflowOrigin"),
    "WorkflowOriginKind": ("task_center.workflow.state", "WorkflowOriginKind"),
    "WorkflowStarter": ("task_center.workflow.starter", "WorkflowStarter"),
    "WorkflowStatus": ("task_center.workflow.state", "WorkflowStatus"),
    "PlannedGeneratorTask": ("task_center.submissions", "PlannedGeneratorTask"),
    "PlannerSubmission": ("task_center.submissions", "PlannerSubmission"),
    "RecipeRegistry": (
        "task_center.context_engine.recipes_registry",
        "RecipeRegistry",
    ),
    "StartedWorkflow": ("task_center.workflow.starter", "StartedWorkflow"),
    "TaskCenterInvariantViolation": (
        "task_center._core.primitives",
        "TaskCenterInvariantViolation",
    ),
    "TaskCenterSandboxProvisioner": (
        "task_center.entry",
        "TaskCenterSandboxProvisioner",
    ),
    "TaskCenterEntry": ("task_center.entry.bootstrap", "TaskCenterEntry"),
    "TaskCenterEntryHandle": (
        "task_center.entry.bootstrap",
        "TaskCenterEntryHandle",
    ),
    "ordered_generator_tasks": (
        "task_center.attempt.generator_dag",
        "ordered_generator_tasks",
    ),
    "start_task_center_run": (
        "task_center.entry.bootstrap",
        "start_task_center_run",
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
