"""TaskCenter entry bootstrap for top-level user requests.

The entry layer is a service boundary, not an agent role. It creates the
request/run/sandbox/runtime shell, converts the prompt into a normal Workflow, and
lets the Workflow -> Iteration -> Attempt lifecycle launch the planner.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING

from agents import validate_agent_definitions_resolved
from db.stores import (
    AttemptStore,
    ContextPacketStore,
    WorkflowStore,
    IterationStore,
    TaskCenterStore,
)
from task_center._core.primitives import TaskCenterLifecycleConfig
from task_center.agent_launch.composer import AgentEntryComposer
from task_center.attempt.launch import (
    AgentStreamEmitter,
    AttemptAgentRunner,
    EphemeralAttemptAgentLauncher,
)
from task_center.attempt.orchestrator_registry import AttemptOrchestratorRegistry
from task_center.attempt.deps import AttemptDeps
from task_center.context_engine.core import ContextEngine, ContextEngineDeps
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.entry.sandbox_provisioning import (
    TaskCenterSandboxBinding,
    TaskCenterSandboxProvisioner,
)
from task_center.workflow.starter import WorkflowStarter
from task_center.workflow.state import WorkflowOrigin
from task_center.iteration import OpenIterationCoordinatorRegistry

if TYPE_CHECKING:
    from runtime.app_factory import RuntimeConfig


@dataclass(frozen=True, slots=True)
class TaskCenterEntryHandle:
    request_id: str
    task_center_run_id: str
    binding: TaskCenterSandboxBinding
    workflow_id: str
    initial_iteration_id: str
    initial_attempt_id: str
    launcher: EphemeralAttemptAgentLauncher

    @property
    def sandbox_id(self) -> str:
        return self.binding.sandbox_id


def start_task_center_run(
    *,
    config: RuntimeConfig,
    prompt: str,
    sandbox_id: str | None,
    on_agent_event: AgentStreamEmitter | None,
    task_store: TaskCenterStore,
    workflow_store: WorkflowStore,
    iteration_store: IterationStore,
    attempt_store: AttemptStore,
    runner: AttemptAgentRunner | None = None,
    context_packet_store: ContextPacketStore | None = None,
    sandbox_provisioner: TaskCenterSandboxProvisioner | None = None,
) -> TaskCenterEntryHandle:
    """Start a TaskCenter run by converting *prompt* into the first Workflow."""
    return TaskCenterEntry(
        config=config,
        prompt=prompt,
        sandbox_id=sandbox_id,
        on_agent_event=on_agent_event,
        task_store=task_store,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        runner=runner,
        context_packet_store=context_packet_store,
        sandbox_provisioner=sandbox_provisioner,
    ).start()


class TaskCenterEntry:
    """Bootstraps a top-level prompt into the normal Workflow lifecycle."""

    def __init__(
        self,
        *,
        config: RuntimeConfig,
        prompt: str,
        sandbox_id: str | None,
        on_agent_event: AgentStreamEmitter | None,
        task_store: TaskCenterStore,
        workflow_store: WorkflowStore,
        iteration_store: IterationStore,
        attempt_store: AttemptStore,
        runner: AttemptAgentRunner | None = None,
        context_packet_store: ContextPacketStore | None = None,
        sandbox_provisioner: TaskCenterSandboxProvisioner | None = None,
    ) -> None:
        self._config = config
        self._prompt = prompt
        self._sandbox_id = sandbox_id
        self._on_agent_event = on_agent_event
        self._task_store = task_store
        self._workflow_store = workflow_store
        self._iteration_store = iteration_store
        self._attempt_store = attempt_store
        self._runner = runner
        self._context_packet_store = context_packet_store
        self._sandbox_provisioner = sandbox_provisioner or TaskCenterSandboxProvisioner()

    def start(self) -> TaskCenterEntryHandle:
        _assert_stores_ready(
            task_store=self._task_store,
            workflow_store=self._workflow_store,
            iteration_store=self._iteration_store,
            attempt_store=self._attempt_store,
        )
        request_id, run_id, binding = self._create_top_level_run()
        iteration_coordinators = OpenIterationCoordinatorRegistry()
        runtime, launcher = self._create_runtime(iteration_coordinators=iteration_coordinators)

        try:
            started = WorkflowStarter(runtime=runtime).start(
                prompt=self._prompt,
                origin=WorkflowOrigin.entry(task_center_run_id=run_id),
            )
        except Exception:
            self._finish_run_if_open(run_id, status="failed")
            raise

        return TaskCenterEntryHandle(
            request_id=request_id,
            task_center_run_id=run_id,
            binding=binding,
            workflow_id=started.workflow_id,
            initial_iteration_id=started.initial_iteration_id,
            initial_attempt_id=started.initial_attempt_id,
            launcher=launcher,
        )

    def _create_top_level_run(self) -> tuple[str, str, TaskCenterSandboxBinding]:
        request_id = str(uuid.uuid4())
        run_id = str(uuid.uuid4())
        binding = self._sandbox_provisioner.prepare_for_run(
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
        return request_id, run_id, binding

    def _create_runtime(
        self, *, iteration_coordinators: OpenIterationCoordinatorRegistry
    ) -> tuple[AttemptDeps, EphemeralAttemptAgentLauncher]:
        runtime_ref: AttemptDeps | None = None
        launcher = EphemeralAttemptAgentLauncher(
            config=self._config,
            deps_provider=lambda: runtime_ref,
            sandbox_id=self._sandbox_id,
            on_event=self._on_agent_event,
            runner=self._runner,
        )
        runtime = AttemptDeps(
            workflow_store=self._workflow_store,
            iteration_store=self._iteration_store,
            attempt_store=self._attempt_store,
            task_store=self._task_store,
            agent_launcher=launcher,
            orchestrator_registry=AttemptOrchestratorRegistry(),
            iteration_coordinators=iteration_coordinators,
            lifecycle_config=TaskCenterLifecycleConfig(),
            composer=self._build_composer(),
        )
        runtime_ref = runtime
        return runtime, launcher

    def _build_composer(self) -> AgentEntryComposer:
        register_builtin_recipes()
        validate_agent_definitions_resolved()
        deps = ContextEngineDeps(
            workflow_store=self._workflow_store,
            iteration_store=self._iteration_store,
            attempt_store=self._attempt_store,
            task_store=self._task_store,
            context_packet_store=self._context_packet_store,
        )
        return AgentEntryComposer.default(ContextEngine(deps))

    def _finish_run_if_open(self, run_id: str, *, status: str) -> None:
        run = self._task_store.get_run(run_id)
        if run is not None and run.get("status") not in ("done", "failed"):
            self._task_store.finish_run(run_id, status=status)


def _assert_stores_ready(
    *,
    task_store: TaskCenterStore,
    workflow_store: WorkflowStore,
    iteration_store: IterationStore,
    attempt_store: AttemptStore,
) -> None:
    if not (
        task_store.is_ready
        and workflow_store.is_ready
        and iteration_store.is_ready
        and attempt_store.is_ready
    ):
        raise RuntimeError("TaskCenter stores are not ready.")
