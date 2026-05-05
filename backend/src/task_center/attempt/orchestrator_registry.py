"""Process-local registry for active harness graph orchestrators."""

from __future__ import annotations

from typing import TYPE_CHECKING

from task_center.exceptions import GraphInvariantViolation

if TYPE_CHECKING:
    from task_center.attempt.orchestrator import HarnessGraphOrchestrator


class HarnessGraphOrchestratorRegistry:
    """In-memory lookup by HarnessGraph id."""

    def __init__(self) -> None:
        self._by_graph_id: dict[str, "HarnessGraphOrchestrator"] = {}

    def register(self, orchestrator: "HarnessGraphOrchestrator") -> None:
        graph_id = orchestrator.harness_graph_id
        current = self._by_graph_id.get(graph_id)
        if current is not None and current is not orchestrator:
            raise GraphInvariantViolation(
                f"HarnessGraphOrchestrator already registered for graph "
                f"{graph_id!r}"
            )
        self._by_graph_id[graph_id] = orchestrator

    def get(self, harness_graph_id: str) -> "HarnessGraphOrchestrator | None":
        return self._by_graph_id.get(harness_graph_id)

    def get_or_raise(self, harness_graph_id: str) -> "HarnessGraphOrchestrator":
        orchestrator = self.get(harness_graph_id)
        if orchestrator is None:
            raise GraphInvariantViolation(
                f"No active HarnessGraphOrchestrator for graph "
                f"{harness_graph_id!r}"
            )
        return orchestrator

    def deregister(self, harness_graph_id: str) -> None:
        self._by_graph_id.pop(harness_graph_id, None)
