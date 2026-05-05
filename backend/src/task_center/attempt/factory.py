"""Composition helper for attempt orchestrator factories."""

from __future__ import annotations

from collections.abc import Callable

from task_center.attempt.state import HarnessGraph
from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.attempt.runtime import HarnessGraphRuntime


def make_attempt_orchestrator_factory(
    *,
    runtime: HarnessGraphRuntime,
) -> Callable[[HarnessGraph, Callable[[str], None]], HarnessGraphOrchestrator]:
    def factory(
        graph: HarnessGraph,
        on_graph_closed: Callable[[str], None],
    ) -> HarnessGraphOrchestrator:
        orchestrator = HarnessGraphOrchestrator(
            harness_graph=graph,
            on_graph_closed=on_graph_closed,
            runtime=runtime,
        )
        return orchestrator

    return factory
