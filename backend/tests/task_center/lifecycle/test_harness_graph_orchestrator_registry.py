"""HarnessGraphOrchestratorRegistry tests."""

from __future__ import annotations

import pytest

from task_center.exceptions import GraphInvariantViolation
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)


class _FakeOrchestrator:
    def __init__(self, graph_id: str) -> None:
        self.harness_graph_id = graph_id


def test_registry_enforces_one_orchestrator_per_graph():
    registry = HarnessGraphOrchestratorRegistry()
    registry.register(_FakeOrchestrator("g1"))  # type: ignore[arg-type]

    with pytest.raises(GraphInvariantViolation):
        registry.register(_FakeOrchestrator("g1"))  # type: ignore[arg-type]


def test_registry_deregister_allows_replacement():
    registry = HarnessGraphOrchestratorRegistry()
    first = _FakeOrchestrator("g1")
    second = _FakeOrchestrator("g1")

    registry.register(first)  # type: ignore[arg-type]
    registry.deregister("g1")
    registry.register(second)  # type: ignore[arg-type]

    assert registry.get("g1") is second
