"""TaskCenter entrypoint for top-level user requests."""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING

from db.stores import (
    ComplexTaskRequestStore,
    HarnessGraphStore,
    TaskCenterStore,
    TaskSegmentStore,
)
from task_center.complex_task.handler import ComplexTaskRequestHandler
from task_center.complex_task.request import ComplexTaskCloseReport
from task_center.config import HarnessLifecycleConfig
from task_center.harness_graph.factory import make_harness_graph_orchestrator_factory
from task_center.harness_graph.graph import (
    HarnessGraphFailReason,
    HarnessGraphStatus,
)
from task_center.harness_graph.entry_builder import (
    ENTRY_AGENT_NAME,
    EntryHarnessGraph,
    EntryHarnessGraphBuilder,
)
from task_center.harness_graph.launcher import (
    AgentStreamEmitter,
    EphemeralHarnessAgentLauncher,
    HarnessAgentRunner,
)
from task_center.harness_graph.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.harness_graph.runtime import HarnessAgentLaunch, HarnessGraphRuntime
from task_center.segment.registry import SegmentManagerRegistry
from task_center.task import HarnessTaskRole

if TYPE_CHECKING:
    from server.app_factory import RuntimeConfig


@dataclass(frozen=True, slots=True)
class TaskCenterEntryHandle:
    request_id: str
    task_center_run_id: str
    complex_task_request_id: str
    task_segment_id: str
    harness_graph_id: str
    entry_task_id: str
    launcher: EphemeralHarnessAgentLauncher


def start_task_center_entry_run(
    *,
    config: "RuntimeConfig",
    prompt: str,
    sandbox_id: str | None,
    on_agent_event: AgentStreamEmitter | None,
    task_store: TaskCenterStore,
    request_store: ComplexTaskRequestStore,
    segment_store: TaskSegmentStore,
    graph_store: HarnessGraphStore,
    runner: HarnessAgentRunner | None = None,
) -> TaskCenterEntryHandle:
    """Create a graph-scoped executor entry task for a user request."""
    return TaskCenterEntryCoordinator(
        config=config,
        prompt=prompt,
        sandbox_id=sandbox_id,
        on_agent_event=on_agent_event,
        task_store=task_store,
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        runner=runner,
    ).start()


class TaskCenterEntryCoordinator:
    """Coordinates top-level request startup into the TaskCenter runtime."""

    def __init__(
        self,
        *,
        config: "RuntimeConfig",
        prompt: str,
        sandbox_id: str | None,
        on_agent_event: AgentStreamEmitter | None,
        task_store: TaskCenterStore,
        request_store: ComplexTaskRequestStore,
        segment_store: TaskSegmentStore,
        graph_store: HarnessGraphStore,
        runner: HarnessAgentRunner | None = None,
    ) -> None:
        self._config = config
        self._prompt = prompt
        self._sandbox_id = sandbox_id
        self._on_agent_event = on_agent_event
        self._task_store = task_store
        self._request_store = request_store
        self._segment_store = segment_store
        self._graph_store = graph_store
        self._runner = runner

    def start(self) -> TaskCenterEntryHandle:
        """Create and launch the entry executor graph."""
        self._assert_stores_ready()
        request_id, run_id, entry_task_id = self._create_top_level_run()
        runtime, launcher, manager_registry = self._create_runtime()
        handler = self._create_request_handler(
            runtime=runtime,
            manager_registry=manager_registry,
            task_center_run_id=run_id,
        )
        complex_request = handler.create_complex_task_request(
            task_center_run_id=run_id,
            requested_by_task_id=entry_task_id,
            goal=self._prompt,
        )
        entry_graph = EntryHarnessGraphBuilder(
            runtime=runtime,
            graph_store=self._graph_store,
            task_store=self._task_store,
        ).create(
            handler=handler,
            complex_task_request_id=complex_request.id,
            task_center_run_id=run_id,
            entry_task_id=entry_task_id,
            prompt=self._prompt,
        )
        self._launch_entry_executor(
            runtime=runtime,
            entry_graph=entry_graph,
            task_center_run_id=run_id,
        )
        return TaskCenterEntryHandle(
            request_id=request_id,
            task_center_run_id=run_id,
            complex_task_request_id=complex_request.id,
            task_segment_id=entry_graph.segment_id,
            harness_graph_id=entry_graph.graph_id,
            entry_task_id=entry_graph.task_id,
            launcher=launcher,
        )

    def _assert_stores_ready(self) -> None:
        _assert_stores_ready(
            task_store=self._task_store,
            request_store=self._request_store,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
        )

    def _create_top_level_run(self) -> tuple[str, str, str]:
        request_id = str(uuid.uuid4())
        run_id = str(uuid.uuid4())
        entry_task_id = f"{run_id}:entry"
        self._task_store.create_request(
            request_id=request_id,
            cwd=self._config.cwd,
            sandbox_id=self._sandbox_id,
            request_prompt=self._prompt,
        )
        self._task_store.create_run(
            task_center_run_id=run_id,
            request_id=request_id,
        )
        return request_id, run_id, entry_task_id

    def _create_runtime(
        self,
    ) -> tuple[
        HarnessGraphRuntime,
        EphemeralHarnessAgentLauncher,
        SegmentManagerRegistry,
    ]:
        runtime_ref: HarnessGraphRuntime | None = None
        launcher = EphemeralHarnessAgentLauncher(
            config=self._config,
            runtime=lambda: runtime_ref,
            sandbox_id=self._sandbox_id,
            on_event=self._on_agent_event,
            runner=self._runner,
        )
        manager_registry = SegmentManagerRegistry()
        runtime = HarnessGraphRuntime(
            request_store=self._request_store,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
            task_store=self._task_store,
            agent_launcher=launcher,
            orchestrator_registry=HarnessGraphOrchestratorRegistry(),
            manager_registry=manager_registry,
            lifecycle_config=HarnessLifecycleConfig(),
        )
        runtime_ref = runtime
        return runtime, launcher, manager_registry

    def _create_request_handler(
        self,
        *,
        runtime: HarnessGraphRuntime,
        manager_registry: SegmentManagerRegistry,
        task_center_run_id: str,
    ) -> ComplexTaskRequestHandler:
        def _finish_entry_run(report: ComplexTaskCloseReport) -> None:
            status = "done" if report.outcome == "success" else "failed"
            self._task_store.finish_run(task_center_run_id, status=status)

        return ComplexTaskRequestHandler(
            request_store=self._request_store,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
            manager_registry=manager_registry,
            config=runtime.lifecycle_config,
            deliver_close_report=_finish_entry_run,
            orchestrator_factory=make_harness_graph_orchestrator_factory(
                runtime=runtime,
            ),
            task_store=self._task_store,
        )

    def _launch_entry_executor(
        self,
        *,
        runtime: HarnessGraphRuntime,
        entry_graph: EntryHarnessGraph,
        task_center_run_id: str,
    ) -> None:
        try:
            runtime.agent_launcher.launch(
                HarnessAgentLaunch(
                    task_id=entry_graph.task_id,
                    task_center_run_id=task_center_run_id,
                    harness_graph_id=entry_graph.graph_id,
                    role=HarnessTaskRole.GENERATOR,
                    agent_name=ENTRY_AGENT_NAME,
                    task_input=self._prompt,
                    needs=(),
                )
            )
        except Exception:
            self._graph_store.close(
                entry_graph.graph_id,
                status=HarnessGraphStatus.FAILED,
                fail_reason=HarnessGraphFailReason.STARTUP_FAILED,
            )
            entry_graph.manager.handle_harness_graph_closed(entry_graph.graph_id)
            raise


def _assert_stores_ready(
    *,
    task_store: TaskCenterStore,
    request_store: ComplexTaskRequestStore,
    segment_store: TaskSegmentStore,
    graph_store: HarnessGraphStore,
) -> None:
    if not (
        task_store.is_ready
        and request_store.is_ready
        and segment_store.is_ready
        and graph_store.is_ready
    ):
        raise RuntimeError("TaskCenter stores are not ready.")
