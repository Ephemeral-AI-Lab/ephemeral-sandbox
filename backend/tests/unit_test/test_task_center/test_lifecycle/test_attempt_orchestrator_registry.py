"""AttemptOrchestratorRegistry tests."""

from __future__ import annotations

import pytest

from task_center._core.types import TaskCenterInvariantViolation
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)


class _FakeOrchestrator:
    def __init__(self, attempt_id: str) -> None:
        self.attempt_id = attempt_id


def test_registry_enforces_one_orchestrator_per_graph():
    registry = AttemptOrchestratorRegistry()
    registry.register(_FakeOrchestrator("g1"))  # type: ignore[arg-type]

    with pytest.raises(TaskCenterInvariantViolation):
        registry.register(_FakeOrchestrator("g1"))  # type: ignore[arg-type]


def test_registry_deregister_allows_replacement():
    registry = AttemptOrchestratorRegistry()
    first = _FakeOrchestrator("g1")
    second = _FakeOrchestrator("g1")

    registry.register(first)  # type: ignore[arg-type]
    registry.deregister("g1")
    registry.register(second)  # type: ignore[arg-type]

    assert registry.get("g1") is second
