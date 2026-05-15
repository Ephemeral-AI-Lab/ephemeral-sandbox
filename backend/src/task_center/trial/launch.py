"""Production launcher + LaunchBuilder for TaskCenter harness agents.

Phase 7d merger: bundles the former ``attempt/launcher.py`` (run-exhaustion
reporting + EphemeralTrialAgentLauncher) and ``attempt/launch_builder.py``
(role-specific AgentLaunch construction) into one module.
"""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

from agents import get_definition
from message.stream_events import StreamEvent
from task_center.trial.runtime import AgentLaunch, TrialDeps
from task_center.trial.state import TrialFailReason, TrialStatus
from task_center.context_engine.scope import ContextScope
from task_center._core.types import TaskCenterInvariantViolation
from task_center.task_state import (
    EvaluatorSubmission,
    GeneratorSubmission,
    PlannerFailureSubmission,
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
)
from tools import ExecutionMetadata

if TYPE_CHECKING:
    from agents import AgentDefinition
    from runtime.app_factory import RuntimeConfig
    from task_center.trial.orchestrator import TrialOrchestrator
    from task_center.trial.state import Trial
    from task_center.trial.contexts import LaunchCtx

logger = logging.getLogger(__name__)

TrialDepsProvider = Callable[[], TrialDeps | None]
AttemptAgentRunner = Callable[..., Awaitable[Any]]
AgentStreamEmitter = Callable[[StreamEvent], Awaitable[None]]


class EphemeralTrialAgentLauncher:
    """Schedules trial-scoped ephemeral agents and reports run exhaustion.

    Terminal submission tools mutate the harness trial during the agent run.
    If an agent exits or crashes while its task is still ``running``, the
    launcher synthesizes the matching harness failure submission so lifecycle
    ownership remains in the orchestrator.
    """

    def __init__(
        self,
        *,
        config: RuntimeConfig,
        runtime: TrialDepsProvider,
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
        agent_def = get_definition(launch.agent_name)
        if agent_def is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter agent definition {launch.agent_name!r} is not registered."
            )
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError as exc:
            raise TaskCenterInvariantViolation(
                "TaskCenter agent launcher requires an active asyncio event loop."
            ) from exc

        task = loop.create_task(self._run_launch(launch, agent_def))
        self._pending.add(task)
        task.add_done_callback(self._pending.discard)

    async def wait_for_idle(self) -> None:
        """Wait until all currently scheduled and recursively spawned runs finish."""
        while self._pending:
            pending = tuple(self._pending)
            await asyncio.gather(*pending)
            self._pending.difference_update(task for task in pending if task.done())
            await asyncio.sleep(0)

    async def _run_launch(
        self,
        launch: AgentLaunch,
        agent_def: AgentDefinition,
    ) -> None:
        runtime = self._runtime()
        if runtime is None:
            raise TaskCenterInvariantViolation("TaskCenter attempt runtime is not initialized.")
        runner = self._runner
        if runner is None:
            from engine.api import run_ephemeral_agent

            runner = run_ephemeral_agent

        # Runtime is always attached: attempt-mode tools resolve via the
        # attempt id, entry-mode tools branch on ``runtime.entry_task_controller``.
        metadata = ExecutionMetadata(
            task_center_run_id=launch.task_center_run_id,
            task_center_task_id=launch.task_id,
            task_center_attempt_id=launch.attempt_id,
            task_center_mission_id=launch.goal_id,
            task_center_request_id=launch.goal_id,
            attempt_runtime=runtime,
            composer=runtime.composer,
        )
        try:
            result: Any = await runner(
                self._config,
                launch.rendered_prompt,
                agent_def=agent_def,
                sandbox_id=self._sandbox_id,
                persist_agent_run=True,
                task_id=launch.task_id,
                on_event=self._on_event,
                extra_tool_metadata=metadata,
            )
        except Exception as exc:  # pragma: no cover - defensive runner boundary
            logger.exception(
                "EphemeralTrialAgentLauncher: agent run failed",
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

        # Guard the public-seam contract: ``AttemptAgentRunner`` is typed
        # ``Callable[..., Awaitable[Any]]`` so any value (including ``None``
        # or an object missing ``.status``) is permitted. Treat those as the
        # same exhaustion case so ``wait_for_idle`` does not propagate
        # ``AttributeError`` out of the asyncio task.
        if result is None:
            await self._report_unfinished_running_task(
                launch,
                summary="Agent runner returned None.",
            )
            return

        status = getattr(result, "status", None)
        if status == "failed":
            error = getattr(result, "error", None) or "unknown error"
            summary = f"Agent run failed: {error}"
        else:
            summary = "Agent run ended without a terminal submission."
        await self._report_unfinished_running_task(launch, summary=summary)

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
        if task is None or task.get("status") != TaskCenterTaskStatus.RUNNING.value:
            # Entry-mode tasks may already be in WAITING_MISSION after a
            # delegated mission start; or DONE/FAILED via a terminal. Either way,
            # the lifecycle owner has already moved the task off RUNNING.
            return

        _report_exhaustion(self, runtime, launch, summary=summary)


_ROLE_FAIL_REASONS: dict[TaskCenterTaskRole, TrialFailReason] = {
    TaskCenterTaskRole.PLANNER: TrialFailReason.PLANNER_FAILED,
    TaskCenterTaskRole.GENERATOR: TrialFailReason.GENERATOR_FAILED,
    TaskCenterTaskRole.EVALUATOR: TrialFailReason.EVALUATOR_FAILED,
}


def _fail_unowned_attempt(
    runtime: TrialDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> None:
    """Close task + trial directly when the orchestrator is missing."""
    logger.error(
        "EphemeralTrialAgentLauncher: missing orchestrator for unfinished task",
        extra={"task_id": launch.task_id, "attempt_id": launch.attempt_id},
    )
    runtime.task_store.set_task_status(
        launch.task_id,
        status=TaskCenterTaskStatus.FAILED.value,
        summary={"fail_reason": "run_exhausted", "summary": summary},
    )
    if launch.attempt_id is None:
        return
    trial = runtime.trial_store.get(launch.attempt_id)
    if trial is None or trial.is_closed:
        return
    runtime.trial_store.close(
        trial.id,
        status=TrialStatus.FAILED,
        fail_reason=_ROLE_FAIL_REASONS[launch.role],
        closed_at=datetime.now(UTC),
    )
    manager_registry = runtime.manager_registry
    if manager_registry is None:
        return
    manager = manager_registry.get(trial.iteration_id)
    if manager is not None:
        manager.handle_attempt_closed(trial.id)


def _require_attempt_orchestrator(
    launcher: EphemeralTrialAgentLauncher,
    runtime: TrialDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> TrialOrchestrator | None:
    if launch.attempt_id is None:
        raise TaskCenterInvariantViolation(
            f"Role {launch.role!r} exhaustion report requires launch.attempt_id."
        )
    orchestrator = runtime.orchestrator_registry.get(launch.attempt_id)
    if orchestrator is None:
        _fail_unowned_attempt(runtime, launch, summary=summary)
        return None
    return orchestrator


def _report_exhaustion(
    launcher: EphemeralTrialAgentLauncher,
    runtime: TrialDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> None:
    """Single role-parameterized exhaustion reporter."""
    if launch.role == TaskCenterTaskRole.ENTRY_EXECUTOR:
        controller = runtime.entry_task_controller
        if controller is None:
            _fail_unowned_attempt(runtime, launch, summary=summary)
            return
        controller.apply_run_exhausted(summary=summary)
        return

    orchestrator = _require_attempt_orchestrator(launcher, runtime, launch, summary=summary)
    if orchestrator is None:
        return

    attempt_id = launch.attempt_id or ""
    if launch.role == TaskCenterTaskRole.PLANNER:
        orchestrator.apply_planner_failure(
            PlannerFailureSubmission(
                attempt_id=attempt_id,
                planner_task_id=launch.task_id,
                fail_reason="run_exhausted",
                summary=summary,
            )
        )
    elif launch.role == TaskCenterTaskRole.GENERATOR:
        orchestrator.apply_generator_submission(
            GeneratorSubmission(
                attempt_id=attempt_id,
                task_id=launch.task_id,
                outcome="failure",
                summary=summary,
                payload={"fail_reason": "run_exhausted"},
            )
        )
    elif launch.role == TaskCenterTaskRole.EVALUATOR:
        orchestrator.apply_evaluator_submission(
            EvaluatorSubmission(
                attempt_id=attempt_id,
                task_id=launch.task_id,
                outcome="failure",
                summary=summary,
                payload={"fail_reason": "run_exhausted"},
            )
        )
    else:
        raise TaskCenterInvariantViolation(
            f"No exhaustion reporter for role {launch.role!r}"
        )


# ---- LaunchBuilder (role-parametrized AgentLaunch factory) -----------------


PLANNER_AGENT_NAME = "planner"
EVALUATOR_AGENT_NAME = "evaluator"


@dataclass(frozen=True, slots=True)
class LaunchBuilder:
    """Build :class:`AgentLaunch` records for each harness role."""

    runtime: LaunchCtx

    def for_planner(self, *, attempt: Trial, task_id: str) -> AgentLaunch:
        iteration = self._require_iteration(attempt)
        return self._build(
            role=TaskCenterTaskRole.PLANNER,
            base_agent_name=PLANNER_AGENT_NAME,
            scope=ContextScope.for_planner(
                goal_id=iteration.goal_id,
                iteration_id=iteration.id,
                attempt_id=attempt.id,
            ),
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            needs=(),
            mission_id=iteration.goal_id,
        )

    def for_generator(
        self,
        *,
        attempt: Trial,
        task: dict[str, Any],
        base_agent_name: str,
    ) -> AgentLaunch:
        iteration = self._require_iteration(attempt)
        task_id = str(task["id"])
        return self._build(
            role=TaskCenterTaskRole.GENERATOR,
            base_agent_name=base_agent_name,
            scope=ContextScope.for_generator(
                goal_id=iteration.goal_id,
                iteration_id=iteration.id,
                attempt_id=attempt.id,
                task_id=task_id,
            ),
            task_id=task_id,
            task_center_run_id=task["task_center_run_id"],
            attempt_id=attempt.id,
            needs=tuple(task["needs"]),
            mission_id=iteration.goal_id,
        )

    def for_evaluator(self, *, attempt: Trial, task_id: str) -> AgentLaunch:
        iteration = self._require_iteration(attempt)
        return self._build(
            role=TaskCenterTaskRole.EVALUATOR,
            base_agent_name=EVALUATOR_AGENT_NAME,
            scope=ContextScope.for_evaluator(
                goal_id=iteration.goal_id,
                iteration_id=iteration.id,
                attempt_id=attempt.id,
            ),
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            needs=tuple(attempt.generator_task_ids),
            mission_id=iteration.goal_id,
        )

    def for_entry(
        self,
        *,
        task_id: str,
        task_center_run_id: str,
        base_agent_name: str,
    ) -> AgentLaunch:
        return self._build(
            role=TaskCenterTaskRole.ENTRY_EXECUTOR,
            base_agent_name=base_agent_name,
            scope=ContextScope.for_entry_executor(task_id=task_id),
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            attempt_id=None,
            needs=(),
            mission_id=None,
        )

    def _build(
        self,
        *,
        role: TaskCenterTaskRole,
        base_agent_name: str,
        scope: ContextScope,
        task_id: str,
        task_center_run_id: str,
        attempt_id: str | None,
        needs: tuple[str, ...],
        mission_id: str | None,
    ) -> AgentLaunch:
        bundle = self.runtime.require_composer().compose(
            base_agent_name=base_agent_name, scope=scope
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            attempt_id=attempt_id,
            role=role,
            agent_name=bundle.agent_def.name,
            rendered_prompt=bundle.rendered_prompt,
            needs=needs,
            context_packet_id=bundle.context_packet_id,
            goal_id=mission_id,
        )

    def _require_iteration(self, attempt: Trial) -> Any:
        iteration = self.runtime.iteration_store.get(attempt.iteration_id)
        if iteration is None:
            raise TaskCenterInvariantViolation(
                f"Iteration {attempt.iteration_id!r} not found"
            )
        return iteration
