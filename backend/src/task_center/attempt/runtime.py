"""Runtime dependency seam for harness graph orchestration."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Protocol

from db.stores.complex_task_request_store import ComplexTaskRequestStore
from db.stores.harness_graph_store import HarnessGraphStore
from db.stores.task_center_store import TaskCenterStore
from db.stores.task_segment_store import TaskSegmentStore
from task_center.config import HarnessLifecycleConfig
from task_center.exceptions import GraphInvariantViolation
from task_center.attempt.state import HarnessGraph
from task_center.episode.registry import SegmentManagerRegistry
from task_center.task import HarnessTaskRole

if TYPE_CHECKING:
    from task_center.context_engine.composer import ContextComposer
    from task_center.entry_task_controller import EntryTaskController
    from task_center.attempt.orchestrator_registry import (
        HarnessGraphOrchestratorRegistry,
    )


@dataclass(frozen=True, slots=True)
class AgentLaunch:
    task_id: str
    task_center_run_id: str
    harness_graph_id: str | None
    role: HarnessTaskRole
    agent_name: str
    task_input: str
    needs: tuple[str, ...]
    system_prompt: str = ""
    context_packet_id: str | None = None
    complex_task_request_id: str | None = None


class HarnessAgentLauncher(Protocol):
    """Launches or queues one harness agent task."""

    def launch(self, launch: AgentLaunch) -> None: ...


@dataclass(frozen=True, slots=True)
class HarnessGraphRuntime:
    request_store: ComplexTaskRequestStore
    segment_store: TaskSegmentStore
    graph_store: HarnessGraphStore
    task_store: TaskCenterStore
    agent_launcher: HarnessAgentLauncher
    orchestrator_registry: "HarnessGraphOrchestratorRegistry"
    manager_registry: SegmentManagerRegistry | None = None
    lifecycle_config: HarnessLifecycleConfig = field(default_factory=HarnessLifecycleConfig)
    # When set, orchestrator + dispatcher route launches through the composer
    # to obtain a rendered task_input + selected agent_def + system_prompt.
    # Optional so existing tests can continue without composer wiring.
    composer: "ContextComposer | None" = None
    # Lifecycle controller for the graph-less entry executor. ``None`` for
    # delegated-only runtimes (mission starter builds its own runtime
    # with no controller because delegated requests always have a graph).
    # The close-report router and launcher use this to dispatch lifecycle
    # events for entry tasks whose ``task_center_harness_graph_id`` is None.
    entry_task_controller: "EntryTaskController | None" = None

    def task_center_run_id_for_graph(self, graph: HarnessGraph) -> str:
        segment = self.segment_store.get(graph.task_segment_id)
        if segment is None:
            raise GraphInvariantViolation(
                f"TaskSegment {graph.task_segment_id!r} not found for "
                f"HarnessGraph {graph.id!r}"
            )
        request = self.request_store.get(segment.complex_task_request_id)
        if request is None:
            raise GraphInvariantViolation(
                f"ComplexTaskRequest {segment.complex_task_request_id!r} not "
                f"found for TaskSegment {segment.id!r}"
            )
        return request.task_center_run_id

    def require_composer(self) -> "ContextComposer":
        if self.composer is None:
            raise GraphInvariantViolation(
                "HarnessGraphRuntime requires a ContextComposer for harness "
                "agent launches; none was wired."
            )
        return self.composer

    def entry_task_controller_for(
        self, task_id: str
    ) -> "EntryTaskController | None":
        """Return the entry controller iff it's bound to *task_id*.

        Used at the four entry-mode dispatch sites (mission starter
        parent-waiting + compensation + duplicate-child check, close-report
        router, submission resolver) so each site collapses to one call
        instead of duplicating the ``is not None and task_id == X`` guard.
        Returns ``None`` for graph-mode tasks or when no controller is
        wired.
        """
        controller = self.entry_task_controller
        if controller is None or controller.task_id != task_id:
            return None
        return controller
