"""Database store layer."""

from __future__ import annotations

from importlib import import_module
from typing import Any

__all__ = [
    "AgentRunStore",
    "WorkflowStore",
    "ContextPacketStore",
    "AttemptStore",
    "ModelStore",
    "TaskCenterStore",
    "IterationStore",
]

_EXPORTS = {
    "AgentRunStore": ("db.stores.agent_run_store", "AgentRunStore"),
    "WorkflowStore": (
        "db.stores.workflow_store",
        "WorkflowStore",
    ),
    "ContextPacketStore": (
        "db.stores.context_packet_store",
        "ContextPacketStore",
    ),
    "AttemptStore": (
        "db.stores.attempt_store",
        "AttemptStore",
    ),
    "ModelStore": ("db.stores.model_store", "ModelStore"),
    "TaskCenterStore": ("db.stores.task_center_store", "TaskCenterStore"),
    "IterationStore": (
        "db.stores.iteration_store",
        "IterationStore",
    ),
}


def __getattr__(name: str) -> Any:
    if name not in _EXPORTS:
        raise AttributeError(name)
    module_name, attr_name = _EXPORTS[name]
    value = getattr(import_module(module_name), attr_name)
    globals()[name] = value
    return value
