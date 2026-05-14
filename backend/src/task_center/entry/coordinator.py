"""TaskCenter entrypoint for top-level user requests.

The entry executor is not a Mission. It is the top-level user-request agent
that can either complete directly or call ``submit_execution_handoff`` to start
the first delegated Mission. Lifecycle events flow through
:class:`EntryTaskController`, which is attached to
:class:`AttemptDeps.entry_task_controller` so the launcher, close-report
router, and submission tools can dispatch entry-mode events consistently.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING

from db.stores import (
    MissionStore,
    ContextPacketStore,
    AttemptStore,
    TaskCenterStore,
    EpisodeStore,
)
from agents import validate_agent_definitions_resolved
from task_center.config import TaskCenterLifecycleConfig
from task_center.agent_launch.composer import ContextComposer
from task_center.context_engine.engine import ContextEngine, ContextEngineDeps
from task_center.agent_launch.predicates import register_builtin_predicates
from task_center.context_engine.recipes import register_builtin_recipes
from task_center.context_engine.scope import ContextScope
from task_center.entry.controller import EntryTaskController
from task_center.agent_launch.launcher import (
    AgentStreamEmitter,
    EphemeralAttemptAgentLauncher,
    AttemptAgentRunner,
)
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, AttemptDeps
from task_center.entry.sandbox_bridge import (
    TaskCenterSandboxBinding,
    TaskCenterSandboxBridge,
)
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.task.models import (
    SpawnReason,
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
)

if TYPE_CHECKING:
    from runtime.app_factory import RuntimeConfig


ENTRY_AGENT_NAME = "entry_executor"


@dataclass(frozen=True, slots=True)
class TaskCenterEntryHandle:
    request_id: str
    task_center_run_id: str
    binding: TaskCenterSandboxBinding
    entry_task_id: str
    launcher: EphemeralAttemptAgentLauncher

    @property
    def sandbox_id(self) -> str:
        return self.binding.sandbox_id


def start_task_center_entry_run(
    *,
    config: RuntimeConfig,
    prompt: str,
    sandbox_id: str | None,
    on_agent_event: AgentStreamEmitter | None,
    task_store: TaskCenterStore,
    mission_store: MissionStore,
    episode_store: EpisodeStore,
    attempt_store: AttemptStore,
    runner: AttemptAgentRunner | None = None,
    context_packet_store: ContextPacketStore | None = None,
    sandbox_bridge: TaskCenterSandboxBridge | None = None,
) -> TaskCenterEntryHandle:
    """Create the entry executor task for a user request."""
    return TaskCenterEntryCoordinator(
        config=config,
        prompt=prompt,
        sandbox_id=sandbox_id,
        on_agent_event=on_agent_event,
        task_store=task_store,
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        runner=runner,
        context_packet_store=context_packet_store,
        sandbox_bridge=sandbox_bridge,
    ).start()


class TaskCenterEntryCoordinator:
    """Coordinates top-level request startup into the TaskCenter runtime."""

    def __init__(
        self,
        *,
        config: RuntimeConfig,
        prompt: str,
        sandbox_id: str | None,
        on_agent_event: AgentStreamEmitter | None,
        task_store: TaskCenterStore,
        mission_store: MissionStore,
        episode_store: EpisodeStore,
        attempt_store: AttemptStore,
        runner: AttemptAgentRunner | None = None,
        context_packet_store: ContextPacketStore | None = None,
        sandbox_bridge: TaskCenterSandboxBridge | None = None,
    ) -> None:
        self._config = config
        self._prompt = prompt
        self._sandbox_id = sandbox_id
        self._on_agent_event = on_agent_event
        self._task_store = task_store
        self._mission_store = mission_store
        self._episode_store = episode_store
        self._attempt_store = attempt_store
        self._runner = runner
        self._context_packet_store = context_packet_store
        self._sandbox_bridge = sandbox_bridge or TaskCenterSandboxBridge()

    def start(self) -> TaskCenterEntryHandle:
        """Create and launch the entry executor."""
        self._assert_stores_ready()
        request_id, run_id, entry_task_id, binding = self._create_top_level_run()
        manager_registry = EpisodeManagerRegistry()

        self._write_entry_task_row(
            entry_task_id=entry_task_id,
            task_center_run_id=run_id,
        )
        controller = EntryTaskController(
            task_id=entry_task_id,
            task_center_run_id=run_id,
            task_store=self._task_store,
        )
        runtime, launcher = self._create_runtime(
            manager_registry=manager_registry,
            entry_task_controller=controller,
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
            entry_task_id=entry_task_id,
            launcher=launcher,
        )

    # ---- internal: setup ---------------------------------------------------

    def _assert_stores_ready(self) -> None:
        _assert_stores_ready(
            task_store=self._task_store,
            mission_store=self._mission_store,
            episode_store=self._episode_store,
            attempt_store=self._attempt_store,
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

    def _create_runtime(
        self,
        *,
        manager_registry: EpisodeManagerRegistry,
        entry_task_controller: EntryTaskController,
    ) -> tuple[AttemptDeps, EphemeralAttemptAgentLauncher]:
        runtime_ref: AttemptDeps | None = None
        launcher = EphemeralAttemptAgentLauncher(
            config=self._config,
            runtime=lambda: runtime_ref,
            sandbox_id=self._sandbox_id,
            on_event=self._on_agent_event,
            runner=self._runner,
        )
        composer = self._build_composer()
        runtime = AttemptDeps(
            mission_store=self._mission_store,
            episode_store=self._episode_store,
            attempt_store=self._attempt_store,
            task_store=self._task_store,
            agent_launcher=launcher,
            orchestrator_registry=AttemptOrchestratorRegistry(),
            manager_registry=manager_registry,
            lifecycle_config=TaskCenterLifecycleConfig(),
            composer=composer,
            entry_task_controller=entry_task_controller,
        )
        runtime_ref = runtime
        return runtime, launcher

    def _build_composer(self) -> ContextComposer:
        """Construct the composer + register built-in predicates / recipes.

        Predicate and recipe registration are idempotent — re-registration is
        the intended steady-state behaviour: each entry-coordinator startup
        re-asserts the builtin set and cross-validates every loaded
        :class:`AgentDefinition` so a typo in a frontmatter ``variants:``
        block, a dangling target, a chained variant, or an unknown
        ``context_recipe`` fails the spawn here rather than during the first
        model turn. Tests that intentionally mutate the process-global
        :class:`PredicateRegistry` or :class:`RecipeRegistry` between
        coordinator builds should reset them to a known state in their own
        teardown — this method is not a sandbox.
        """
        register_builtin_predicates()
        register_builtin_recipes()
        validate_agent_definitions_resolved()
        deps = ContextEngineDeps(
            mission_store=self._mission_store,
            episode_store=self._episode_store,
            attempt_store=self._attempt_store,
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
        """Write the entry task row with ``task_center_attempt_id=None``."""
        self._task_store.upsert_task(
            task_id=entry_task_id,
            task_center_run_id=task_center_run_id,
            role=TaskCenterTaskRole.ENTRY_EXECUTOR.value,
            agent_name=ENTRY_AGENT_NAME,
            rendered_prompt=self._prompt,
            status=TaskCenterTaskStatus.RUNNING.value,
            summaries=[],
            needs=[],
            task_center_attempt_id=None,
            spawn_reason=SpawnReason.ENTRY_EXECUTOR.value,
        )

    # ---- internal: launch + cleanup ---------------------------------------

    def _launch_entry_executor(
        self,
        *,
        runtime: AttemptDeps,
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
        runtime: AttemptDeps,
        controller: EntryTaskController,
        task_center_run_id: str,
    ) -> AgentLaunch:
        composer = runtime.require_composer()
        bundle = composer.compose(
            base_agent_name=ENTRY_AGENT_NAME,
            scope=ContextScope(
                task_id=controller.task_id,
            ),
        )
        return AgentLaunch(
            task_id=controller.task_id,
            task_center_run_id=task_center_run_id,
            attempt_id=None,
            role=TaskCenterTaskRole.ENTRY_EXECUTOR,
            agent_name=bundle.agent_def.name,
            rendered_prompt=bundle.rendered_prompt,
            needs=(),
            context_packet_id=bundle.context_packet_id,
            mission_id=None,
        )

    def _compensate_startup_failure(
        self,
        *,
        controller: EntryTaskController,
    ) -> None:
        """Drive the entry stack to FAILED after a launch-time exception."""
        controller.apply_run_exhausted(summary="Entry executor launch failed.")
        run = self._task_store.get_run(controller.task_center_run_id)
        if run is not None and run.get("status") not in ("done", "failed"):
            self._task_store.finish_run(
                controller.task_center_run_id, status="failed"
            )


def _assert_stores_ready(
    *,
    task_store: TaskCenterStore,
    mission_store: MissionStore,
    episode_store: EpisodeStore,
    attempt_store: AttemptStore,
) -> None:
    if not (
        task_store.is_ready
        and mission_store.is_ready
        and episode_store.is_ready
        and attempt_store.is_ready
    ):
        raise RuntimeError("TaskCenter stores are not ready.")
