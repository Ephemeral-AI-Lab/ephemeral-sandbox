"""Production launcher for TaskCenter harness agents."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING

from agents import get_definition
from engine.runtime.lifecycle import EphemeralRunResult
from message.stream_events import StreamEvent
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.attempt.runtime import (
    AgentLaunch,
    AttemptRuntime,
)
from task_center.task import (
    EvaluatorSubmission,
    GeneratorSubmission,
    HarnessTaskRole,
    HarnessTaskStatus,
    PlannerFailureSubmission,
)
from tools import ExecutionMetadata

if TYPE_CHECKING:
    from agents import AgentDefinition
    from server.app_factory import RuntimeConfig
    from task_center.attempt.orchestrator import AttemptOrchestrator

logger = logging.getLogger(__name__)

AttemptRuntimeProvider = Callable[[], AttemptRuntime | None]
AttemptAgentRunner = Callable[..., Awaitable[EphemeralRunResult]]
AgentStreamEmitter = Callable[[StreamEvent], Awaitable[None]]


class EphemeralAttemptAgentLauncher:
    """Schedules attempt-scoped ephemeral agents and reports run exhaustion.

    Terminal submission tools mutate the harness attempt during the agent run.
    If an agent exits or crashes while its task is still ``running``, the
    launcher synthesizes the matching harness failure submission so lifecycle
    ownership remains in the orchestrator.
    """

    def __init__(
        self,
        *,
        config: "RuntimeConfig",
        runtime: AttemptRuntimeProvider,
        sandbox_id: str | None = None,
        on_event: AgentStreamEmitter | None = None,
        runner: AttemptAgentRunner | None = None,
    ) -> None:
        self._config = config
        self._runtime = runtime
        self._sandbox_id = sandbox_id
        self._on_event = on_event
        self._runner = runner
        self._pending: set[asyncio.Task[None]] = set()

    def launch(self, launch: AgentLaunch) -> None:
        agent_def = self._resolve_agent_definition(launch.agent_name)
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError as exc:
            raise TaskCenterInvariantViolation(
                "Harness agent launcher requires an active asyncio event loop."
            ) from exc

        task = loop.create_task(self._run_launch(launch, agent_def))
        self._pending.add(task)
        task.add_done_callback(self._pending.discard)

    async def wait_for_idle(self) -> None:
        """Wait until all currently scheduled and recursively spawned runs finish."""
        while self._pending:
            await asyncio.gather(*tuple(self._pending))

    @staticmethod
    def _resolve_agent_definition(agent_name: str) -> "AgentDefinition":
        agent_def = get_definition(agent_name)
        if agent_def is None:
            raise TaskCenterInvariantViolation(
                f"Harness agent definition {agent_name!r} is not registered."
            )
        return agent_def

    async def _run_launch(
        self,
        launch: AgentLaunch,
        agent_def: "AgentDefinition",
    ) -> None:
        runtime = self._require_runtime()
        runner = self._runner
        if runner is None:
            from engine.runtime.lifecycle import run_ephemeral_agent

            runner = run_ephemeral_agent

        # Runtime is always attached: attempt-mode tools resolve via the
        # attempt id, entry-mode tools branch on ``runtime.entry_task_controller``.
        metadata = ExecutionMetadata(
            task_center_run_id=launch.task_center_run_id,
            task_center_task_id=launch.task_id,
            task_center_attempt_id=launch.attempt_id,
            task_center_request_id=launch.mission_id,
            attempt_runtime=runtime,
            composer=runtime.composer,
        )
        result: EphemeralRunResult | None = None
        try:
            result = await runner(
                self._config,
                launch.task_input,
                agent_def=agent_def,
                sandbox_id=self._sandbox_id,
                persist_agent_run=True,
                task_id=launch.task_id,
                on_event=self._on_event,
                extra_tool_metadata=metadata,
            )
        except Exception as exc:  # pragma: no cover - defensive runner boundary
            logger.exception(
                "EphemeralAttemptAgentLauncher: agent run failed",
                extra={
                    "task_id": launch.task_id,
                    "attempt_id": launch.attempt_id,
                    "agent_name": launch.agent_name,
                },
            )
            await self._report_unfinished_running_task(
                launch,
                summary=f"Agent run crashed: {exc}",
            )
            return

        if result.status == "failed":
            summary = f"Agent run failed: {result.error or 'unknown error'}"
        else:
            summary = "Agent run ended without a terminal submission."
        await self._report_unfinished_running_task(launch, summary=summary)

    def _require_runtime(self) -> AttemptRuntime:
        runtime = self._runtime()
        if runtime is None:
            raise TaskCenterInvariantViolation("Harness attempt runtime is not initialized.")
        return runtime

    async def _report_unfinished_running_task(
        self,
        launch: AgentLaunch,
        *,
        summary: str,
    ) -> None:
        runtime = self._runtime()
        if runtime is None:
            return
        task = runtime.task_store.get_task(launch.task_id)
        if task is None or task.get("status") != HarnessTaskStatus.RUNNING.value:
            # Entry-mode tasks may already be in WAITING_COMPLEX_TASK after a
            # delegated mission start; or DONE/FAILED via a terminal. Either way,
            # the controller has already moved the task off RUNNING and
            # there's nothing to do.
            return

        if launch.attempt_id is None:
            # Entry mode — dispatch through the controller instead of the
            # orchestrator registry. Missing controller is a hard error: the
            # entry task is RUNNING and the run cannot finalize without it.
            controller = runtime.entry_task_controller
            if controller is None:
                self._mark_unowned_task_exhausted(
                    runtime, launch, summary=summary
                )
                return
            controller.apply_run_exhausted(summary=summary)
            return

        orchestrator = runtime.orchestrator_registry.get(launch.attempt_id)
        if orchestrator is None:
            self._mark_unowned_task_exhausted(runtime, launch, summary=summary)
            return

        if launch.role == HarnessTaskRole.PLANNER:
            self._report_planner_exhaustion(orchestrator, launch, summary=summary)
        elif launch.role == HarnessTaskRole.GENERATOR:
            self._report_generator_exhaustion(orchestrator, launch, summary=summary)
        elif launch.role == HarnessTaskRole.EVALUATOR:
            self._report_evaluator_exhaustion(orchestrator, launch, summary=summary)
        else:  # pragma: no cover - exhaustive over HarnessTaskRole
            raise TaskCenterInvariantViolation(f"Unknown harness role: {launch.role!r}")

    @staticmethod
    def _mark_unowned_task_exhausted(
        runtime: AttemptRuntime,
        launch: AgentLaunch,
        *,
        summary: str,
    ) -> None:
        logger.error(
            "EphemeralAttemptAgentLauncher: missing orchestrator for unfinished task",
            extra={
                "task_id": launch.task_id,
                "attempt_id": launch.attempt_id,
            },
        )
        runtime.task_store.set_task_status(
            launch.task_id,
            status=HarnessTaskStatus.FAILED.value,
            summary={"fail_reason": "run_exhausted", "summary": summary},
        )

    @staticmethod
    def _report_planner_exhaustion(
        orchestrator: "AttemptOrchestrator",
        launch: AgentLaunch,
        *,
        summary: str,
    ) -> None:
        orchestrator.apply_planner_failure(
            PlannerFailureSubmission(
                attempt_id=launch.attempt_id,
                planner_task_id=launch.task_id,
                fail_reason="run_exhausted",
                summary=summary,
            )
        )

    @staticmethod
    def _report_generator_exhaustion(
        orchestrator: "AttemptOrchestrator",
        launch: AgentLaunch,
        *,
        summary: str,
    ) -> None:
        orchestrator.apply_generator_submission(
            GeneratorSubmission(
                attempt_id=launch.attempt_id,
                task_id=launch.task_id,
                outcome="failure",
                summary=summary,
                payload={"fail_reason": "run_exhausted"},
            )
        )

    @staticmethod
    def _report_evaluator_exhaustion(
        orchestrator: "AttemptOrchestrator",
        launch: AgentLaunch,
        *,
        summary: str,
    ) -> None:
        orchestrator.apply_evaluator_submission(
            EvaluatorSubmission(
                attempt_id=launch.attempt_id,
                task_id=launch.task_id,
                outcome="failure",
                summary=summary,
                payload={"fail_reason": "run_exhausted"},
            )
        )
