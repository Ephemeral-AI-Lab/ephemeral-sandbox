"""Database store layer."""

from __future__ import annotations

from importlib import import_module
from typing import Any

__all__ = [
    "AgentRunStore",
    "ComplexTaskRequestStore",
    "ContextPacketStore",
    "HarnessGraphStore",
    "ModelStore",
    "TaskCenterStore",
    "TaskSegmentStore",
]

_EXPORTS = {
    "AgentRunStore": ("db.stores.agent_run_store", "AgentRunStore"),
    "ComplexTaskRequestStore": (
        "db.stores.complex_task_request_store",
        "ComplexTaskRequestStore",
    ),
    "ContextPacketStore": (
        "db.stores.context_packet_store",
        "ContextPacketStore",
    ),
    "HarnessGraphStore": (
        "db.stores.harness_graph_store",
        "HarnessGraphStore",
    ),
    "ModelStore": ("db.stores.model_store", "ModelStore"),
    "TaskCenterStore": ("db.stores.task_center_store", "TaskCenterStore"),
    "TaskSegmentStore": (
        "db.stores.task_segment_store",
        "TaskSegmentStore",
    ),
}


def __getattr__(name: str) -> Any:
    if name not in _EXPORTS:
        raise AttributeError(name)
    module_name, attr_name = _EXPORTS[name]
    value = getattr(import_module(module_name), attr_name)
    globals()[name] = value
    return value
