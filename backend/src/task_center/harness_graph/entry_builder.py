"""Entry graph construction for top-level TaskCenter requests."""

from __future__ import annotations

from dataclasses import dataclass

from db.stores import HarnessGraphStore, TaskCenterStore
from task_center.complex_task.handler import ComplexTaskRequestHandler
from task_center.harness_graph.graph import HarnessGraphStage
from task_center.harness_graph.orchestrator import HarnessGraphOrchestrator
from task_center.harness_graph.runtime import HarnessGraphRuntime
from task_center.segment.manager import TaskSegmentManager
from task_center.task import HarnessTaskRole, HarnessTaskStatus

ENTRY_AGENT_NAME = "executor"
ENTRY_SPAWN_REASON = "entry_executor"


@dataclass(frozen=True, slots=True)
class EntryHarnessGraph:
    segment_id: str
    graph_id: str
    task_id: str
    manager: TaskSegmentManager


class EntryHarnessGraphBuilder:
    """Builds the synthetic one-node graph that receives top-level input."""

    def __init__(
        self,
        *,
        runtime: HarnessGraphRuntime,
        graph_store: HarnessGraphStore,
        task_store: TaskCenterStore,
    ) -> None:
        self._runtime = runtime
        self._graph_store = graph_store
        self._task_store = task_store

    def create(
        self,
        *,
        handler: ComplexTaskRequestHandler,
        complex_task_request_id: str,
        task_center_run_id: str,
        entry_task_id: str,
        prompt: str,
    ) -> EntryHarnessGraph:
        segment, manager = handler.create_initial_segment_with_manager(
            complex_task_request_id=complex_task_request_id
        )
        graph = manager.create_initial_harness_graph(start=False)
        self._configure_graph(graph.id, entry_task_id=entry_task_id, prompt=prompt)
        graph = self._graph_store.set_stage(graph.id, HarnessGraphStage.GENERATING)
        self._create_entry_task(
            task_center_run_id=task_center_run_id,
            entry_task_id=entry_task_id,
            graph_id=graph.id,
            prompt=prompt,
        )
        orchestrator = HarnessGraphOrchestrator(
            harness_graph=graph,
            on_graph_closed=manager.handle_harness_graph_closed,
            runtime=self._runtime,
        )
        self._runtime.orchestrator_registry.register(orchestrator)
        return EntryHarnessGraph(
            segment_id=segment.id,
            graph_id=graph.id,
            task_id=entry_task_id,
            manager=manager,
        )

    def _configure_graph(
        self,
        graph_id: str,
        *,
        entry_task_id: str,
        prompt: str,
    ) -> None:
        self._graph_store.set_plan_contract(
            graph_id,
            task_specification=prompt,
            evaluation_criteria=[
                "The entry executor either completes the request directly or "
                "delegates a complex task that closes successfully."
            ],
            continuation_goal=None,
        )
        self._graph_store.set_generator_task_ids(graph_id, [entry_task_id])

    def _create_entry_task(
        self,
        *,
        task_center_run_id: str,
        entry_task_id: str,
        graph_id: str,
        prompt: str,
    ) -> None:
        self._task_store.upsert_task(
            task_id=entry_task_id,
            task_center_run_id=task_center_run_id,
            role=HarnessTaskRole.GENERATOR.value,
            agent_name=ENTRY_AGENT_NAME,
            task_input=prompt,
            status=HarnessTaskStatus.RUNNING.value,
            summaries=[],
            needs=[],
            task_center_harness_graph_id=graph_id,
            spawn_reason=ENTRY_SPAWN_REASON,
        )
