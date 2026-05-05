"""TaskCenter entrypoint for top-level user requests.

The entry executor is launched in **graph-less** mode: it lives in a
:class:`TaskSegment` with zero ``HarnessGraph`` rows (per phase-06
*Sources of truth*: an entry segment may have zero ``HarnessGraph`` rows).
Lifecycle events flow through :class:`EntryTaskController`, which is
attached to :class:`HarnessGraphRuntime.entry_task_controller` so that the
launcher exhaustion path, close-report router, and submission tools can
dispatch into it without knowing whether the spawn was graph-bound or
entry-mode.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING

from db.stores import (
    ComplexTaskRequestStore,
    ContextPacketStore,
    HarnessGraphStore,
    TaskCenterStore,
    TaskSegmentStore,
)
from agents.registry import validate_agent_definitions_resolved
from task_center.mission.handler import ComplexTaskRequestHandler
from task_center.mission.mission import ComplexTaskCloseReport
from task_center.config import HarnessLifecycleConfig
from task_center.context_engine.composer import ContextComposer
from task_center.context_engine.engine import ContextEngine, ContextEngineDeps
from task_center.agent_launch.predicates import register_builtin_predicates
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.context_engine.scope import ContextScope
from task_center.entry_task_controller import EntryTaskController
from task_center.attempt.factory import make_attempt_orchestrator_factory
from task_center.attempt.launcher import (
    AgentStreamEmitter,
    EphemeralHarnessAgentLauncher,
    HarnessAgentRunner,
)
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, HarnessGraphRuntime
from task_center.sandbox_bridge import (
    TaskCenterSandboxBinding,
    TaskCenterSandboxBridge,
)
from task_center.episode.registry import SegmentManagerRegistry
from task_center.task import HarnessTaskRole, HarnessTaskStatus

if TYPE_CHECKING:
    from server.app_factory import RuntimeConfig


ENTRY_AGENT_NAME = "entry_executor"
ENTRY_SPAWN_REASON = "entry_executor"


@dataclass(frozen=True, slots=True)
class TaskCenterEntryHandle:
    request_id: str
    task_center_run_id: str
    binding: TaskCenterSandboxBinding
    complex_task_request_id: str
    task_segment_id: str
    entry_task_id: str
    launcher: EphemeralHarnessAgentLauncher

    @property
    def sandbox_id(self) -> str:
        return self.binding.sandbox_id


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
    context_packet_store: ContextPacketStore | None = None,
    sandbox_bridge: TaskCenterSandboxBridge | None = None,
) -> TaskCenterEntryHandle:
    """Create a graph-less entry executor task for a user request."""
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
        context_packet_store=context_packet_store,
        sandbox_bridge=sandbox_bridge,
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
        context_packet_store: ContextPacketStore | None = None,
        sandbox_bridge: TaskCenterSandboxBridge | None = None,
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
        self._context_packet_store = context_packet_store
        self._sandbox_bridge = sandbox_bridge or TaskCenterSandboxBridge()

    def start(self) -> TaskCenterEntryHandle:
        """Create and launch the entry executor (graph-less)."""
        self._assert_stores_ready()
        request_id, run_id, entry_task_id, binding = self._create_top_level_run()

        manager_registry = SegmentManagerRegistry()
        # The handler created here is reused twice: once to seed the entry
        # request + segment, and again — wrapped on the runtime — to drive
        # delegated mission starts originating from the entry task.
        handler = self._build_request_handler(
            manager_registry=manager_registry,
            task_center_run_id=run_id,
        )
        complex_request = handler.create_mission_request(
            task_center_run_id=run_id,
            requested_by_task_id=entry_task_id,
            goal=self._prompt,
        )
        entry_segment, _segment_manager = (
            handler.create_initial_episode_with_manager(
                complex_task_request_id=complex_request.id,
            )
        )

        controller = EntryTaskController(
            task_id=entry_task_id,
            task_center_run_id=run_id,
            complex_task_request_id=complex_request.id,
            task_segment_id=entry_segment.id,
            task_store=self._task_store,
            segment_store=self._segment_store,
            request_handler=handler,
            manager_registry=manager_registry,
        )
        runtime, launcher = self._create_runtime(
            manager_registry=manager_registry,
            entry_task_controller=controller,
        )
        self._write_entry_task_row(
            entry_task_id=entry_task_id,
            task_center_run_id=run_id,
        )
        self._launch_entry_executor(
            runtime=runtime,
            controller=controller,
            task_center_run_id=run_id,
        )
        return TaskCenterEntryHandle(
            request_id=request_id,
            task_center_run_id=run_id,
            binding=binding,
            complex_task_request_id=complex_request.id,
            task_segment_id=entry_segment.id,
            entry_task_id=entry_task_id,
            launcher=launcher,
        )

    # ---- internal: setup ---------------------------------------------------

    def _assert_stores_ready(self) -> None:
        _assert_stores_ready(
            task_store=self._task_store,
            request_store=self._request_store,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
        )

    def _create_top_level_run(
        self,
    ) -> tuple[str, str, str, TaskCenterSandboxBinding]:
        request_id = str(uuid.uuid4())
        run_id = str(uuid.uuid4())
        entry_task_id = f"{run_id}:entry"
        binding = self._sandbox_bridge.prepare_for_run(
            task_center_run_id=run_id,
            sandbox_id=self._sandbox_id,
        )
        self._sandbox_id = binding.sandbox_id
        self._task_store.create_request(
            request_id=request_id,
            cwd=self._config.cwd,
            sandbox_id=binding.sandbox_id,
            request_prompt=self._prompt,
        )
        self._task_store.create_run(
            task_center_run_id=run_id,
            request_id=request_id,
        )
        return request_id, run_id, entry_task_id, binding

    def _build_request_handler(
        self,
        *,
        manager_registry: SegmentManagerRegistry,
        task_center_run_id: str,
    ) -> ComplexTaskRequestHandler:
        """Build the handler reused for entry-segment + delegated requests.

        ``deliver_close_report`` is the run-finalization callback: when any
        complex_task_request closes (the entry's own, or a delegated child),
        the handler delivers the close report here, which finishes the run.
        """

        def _finish_entry_run(report: ComplexTaskCloseReport) -> None:
            del report  # outcome already persisted to the entry task row
            existing_runs = self._task_store.get_run(task_center_run_id)
            if existing_runs is None or existing_runs.get("status") in (
                "done",
                "failed",
            ):
                return
            entry_task = self._task_store.get_task(
                f"{task_center_run_id}:entry"
            )
            if entry_task is None:
                return
            status = (
                "done"
                if entry_task.get("status") == HarnessTaskStatus.DONE.value
                else "failed"
            )
            self._task_store.finish_run(task_center_run_id, status=status)

        return ComplexTaskRequestHandler(
            request_store=self._request_store,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
            manager_registry=manager_registry,
            config=HarnessLifecycleConfig(),
            deliver_close_report=_finish_entry_run,
            orchestrator_factory=None,  # set below once runtime exists
            task_store=self._task_store,
        )

    def _create_runtime(
        self,
        *,
        manager_registry: SegmentManagerRegistry,
        entry_task_controller: EntryTaskController,
    ) -> tuple[HarnessGraphRuntime, EphemeralHarnessAgentLauncher]:
        runtime_ref: HarnessGraphRuntime | None = None
        launcher = EphemeralHarnessAgentLauncher(
            config=self._config,
            runtime=lambda: runtime_ref,
            sandbox_id=self._sandbox_id,
            on_event=self._on_agent_event,
            runner=self._runner,
        )
        composer = self._build_composer()
        runtime = HarnessGraphRuntime(
            request_store=self._request_store,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
            task_store=self._task_store,
            agent_launcher=launcher,
            orchestrator_registry=HarnessGraphOrchestratorRegistry(),
            manager_registry=manager_registry,
            lifecycle_config=HarnessLifecycleConfig(),
            composer=composer,
            entry_task_controller=entry_task_controller,
        )
        runtime_ref = runtime
        # Late-bind the orchestrator factory on the controller's handler so
        # delegated complex-task requests can spawn real harness graphs.
        entry_task_controller.request_handler.set_orchestrator_factory(
            make_attempt_orchestrator_factory(runtime=runtime)
        )
        return runtime, launcher

    def _build_composer(self) -> ContextComposer:
        """Construct the composer + register built-in predicates / recipes.

        Predicate and recipe registration are idempotent — safe to call once
        per entry-coordinator startup. After registration we cross-validate
        every loaded :class:`AgentDefinition` so a typo in a frontmatter
        ``variants:`` block, a dangling target, a chained variant, or an
        unknown ``context_recipe`` fails the spawn here rather than during
        the first model turn.
        """
        register_builtin_predicates()
        register_builtin_recipes()
        validate_agent_definitions_resolved()
        deps = ContextEngineDeps(
            request_store=self._request_store,
            segment_store=self._segment_store,
            graph_store=self._graph_store,
            task_store=self._task_store,
            context_packet_store=self._context_packet_store,
        )
        return ContextComposer.default(ContextEngine(deps))

    def _write_entry_task_row(
        self,
        *,
        entry_task_id: str,
        task_center_run_id: str,
    ) -> None:
        """Write the entry task row with ``task_center_harness_graph_id=None``."""
        self._task_store.upsert_task(
            task_id=entry_task_id,
            task_center_run_id=task_center_run_id,
            role=HarnessTaskRole.GENERATOR.value,
            agent_name=ENTRY_AGENT_NAME,
            task_input=self._prompt,
            status=HarnessTaskStatus.RUNNING.value,
            summaries=[],
            needs=[],
            task_center_harness_graph_id=None,
            spawn_reason=ENTRY_SPAWN_REASON,
        )

    # ---- internal: launch + cleanup ---------------------------------------

    def _launch_entry_executor(
        self,
        *,
        runtime: HarnessGraphRuntime,
        controller: EntryTaskController,
        task_center_run_id: str,
    ) -> None:
        try:
            launch = self._build_entry_launch(
                runtime=runtime,
                controller=controller,
                task_center_run_id=task_center_run_id,
            )
            if launch.context_packet_id is not None:
                self._task_store.set_task_context_packet_id(
                    controller.task_id,
                    context_packet_id=launch.context_packet_id,
                )
            runtime.agent_launcher.launch(launch)
        except Exception:
            self._compensate_startup_failure(controller=controller)
            raise

    def _build_entry_launch(
        self,
        *,
        runtime: HarnessGraphRuntime,
        controller: EntryTaskController,
        task_center_run_id: str,
    ) -> AgentLaunch:
        composer = runtime.require_composer()
        bundle = composer.compose(
            base_agent_name=ENTRY_AGENT_NAME,
            scope=ContextScope(
                request_id=controller.complex_task_request_id,
                task_id=controller.task_id,
            ),
        )
        return AgentLaunch(
            task_id=controller.task_id,
            task_center_run_id=task_center_run_id,
            harness_graph_id=None,
            role=HarnessTaskRole.GENERATOR,
            agent_name=bundle.agent_def.name,
            task_input=bundle.task_input,
            needs=(),
            system_prompt=bundle.system_prompt,
            context_packet_id=bundle.context_packet_id,
            complex_task_request_id=controller.complex_task_request_id,
        )

    def _compensate_startup_failure(
        self,
        *,
        controller: EntryTaskController,
    ) -> None:
        """Drive the entry stack to FAILED after a launch-time exception."""
        controller.apply_run_exhausted(summary="Entry executor launch failed.")
        # The controller's close-report delivery normally finishes the run.
        # Force-finish here as a safety net for the case where the entry
        # task was already terminal (controller short-circuited) and no
        # close-report fired.
        run = self._task_store.get_run(controller.task_center_run_id)
        if run is not None and run.get("status") not in ("done", "failed"):
            self._task_store.finish_run(
                controller.task_center_run_id, status="failed"
            )


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


__all__ = (
    "ENTRY_AGENT_NAME",
    "ENTRY_SPAWN_REASON",
    "TaskCenterEntryCoordinator",
    "TaskCenterEntryHandle",
    "start_task_center_entry_run",
)
