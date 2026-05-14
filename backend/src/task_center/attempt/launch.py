"""Production launcher + LaunchBuilder for TaskCenter harness agents.

Phase 7d merger: bundles the former ``attempt/launcher.py`` (run-exhaustion
reporting + EphemeralAttemptAgentLauncher) and ``attempt/launch_builder.py``
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
from task_center.attempt.runtime import AgentLaunch, AttemptDeps
from task_center.attempt.state import AttemptFailReason, AttemptStatus
from task_center.context_engine.scope import ContextScope
from task_center.exceptions import TaskCenterInvariantViolation
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
    from task_center.attempt.orchestrator import AttemptOrchestrator
    from task_center.attempt.state import Attempt
    from task_center.contexts import LaunchCtx

logger = logging.getLogger(__name__)

AttemptDepsProvider = Callable[[], AttemptDeps | None]
AttemptAgentRunner = Callable[..., Awaitable[Any]]
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
        config: RuntimeConfig,
        runtime: AttemptDepsProvider,
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

    @staticmethod
    def _resolve_agent_definition(agent_name: str) -> AgentDefinition:
        agent_def = get_definition(agent_name)
        if agent_def is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter agent definition {agent_name!r} is not registered."
            )
        return agent_def

    async def _run_launch(
        self,
        launch: AgentLaunch,
        agent_def: AgentDefinition,
    ) -> None:
        runtime = self._require_runtime()
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
            task_center_mission_id=launch.mission_id,
            task_center_request_id=launch.mission_id,
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

    def _require_runtime(self) -> AttemptDeps:
        runtime = self._runtime()
        if runtime is None:
            raise TaskCenterInvariantViolation("TaskCenter attempt runtime is not initialized.")
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
        if task is None or task.get("status") != TaskCenterTaskStatus.RUNNING.value:
            # Entry-mode tasks may already be in WAITING_MISSION after a
            # delegated mission start; or DONE/FAILED via a terminal. Either way,
            # the lifecycle owner has already moved the task off RUNNING.
            return

        _report_exhaustion(self, runtime, launch, summary=summary)

    @staticmethod
    def _mark_unowned_task_exhausted(
        runtime: AttemptDeps,
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
            status=TaskCenterTaskStatus.FAILED.value,
            summary={"fail_reason": "run_exhausted", "summary": summary},
        )

    @classmethod
    def _fail_unowned_attempt(
        cls,
        runtime: AttemptDeps,
        launch: AgentLaunch,
        *,
        summary: str,
    ) -> None:
        cls._mark_unowned_task_exhausted(runtime, launch, summary=summary)
        if launch.attempt_id is None:
            return
        attempt = runtime.attempt_store.get(launch.attempt_id)
        if attempt is None or attempt.is_closed:
            return
        fail_reason = _fail_reason_for_role(launch.role)
        runtime.attempt_store.close(
            attempt.id,
            status=AttemptStatus.FAILED,
            fail_reason=fail_reason,
            closed_at=datetime.now(UTC),
        )
        manager_registry = runtime.manager_registry
        if manager_registry is None:
            return
        manager = manager_registry.get(attempt.episode_id)
        if manager is not None:
            manager.handle_attempt_closed(attempt.id)

def _require_attempt_orchestrator(
    launcher: EphemeralAttemptAgentLauncher,
    runtime: AttemptDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> AttemptOrchestrator | None:
    if launch.attempt_id is None:
        raise TaskCenterInvariantViolation(
            f"Role {launch.role!r} exhaustion report requires "
            "launch.attempt_id."
        )
    orchestrator = runtime.orchestrator_registry.get(launch.attempt_id)
    if orchestrator is None:
        launcher._fail_unowned_attempt(runtime, launch, summary=summary)
        return None
    return orchestrator


def _report_exhaustion(
    launcher: EphemeralAttemptAgentLauncher,
    runtime: AttemptDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> None:
    """Single role-parameterized exhaustion reporter (lever #6 consolidation).

    Adding a new role means adding one ``elif`` branch here — no dispatch
    table to keep in sync.
    """
    if launch.role == TaskCenterTaskRole.ENTRY_EXECUTOR:
        controller = runtime.entry_task_controller
        if controller is None:
            launcher._mark_unowned_task_exhausted(runtime, launch, summary=summary)
            return
        controller.apply_run_exhausted(summary=summary)
        return

    orchestrator = _require_attempt_orchestrator(
        launcher, runtime, launch, summary=summary
    )
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


_ROLE_FAIL_REASONS: dict[TaskCenterTaskRole, AttemptFailReason] = {
    TaskCenterTaskRole.PLANNER: AttemptFailReason.PLANNER_FAILED,
    TaskCenterTaskRole.GENERATOR: AttemptFailReason.GENERATOR_FAILED,
    TaskCenterTaskRole.EVALUATOR: AttemptFailReason.EVALUATOR_FAILED,
}


def _fail_reason_for_role(role: TaskCenterTaskRole) -> AttemptFailReason:
    """Map an attempt-scoped role to its :class:`AttemptFailReason`.

    ``ENTRY_EXECUTOR`` is intentionally absent: entry tasks have no parent
    attempt, so callers (``_fail_unowned_attempt``) short-circuit before
    they need a fail reason.
    """
    try:
        return _ROLE_FAIL_REASONS[role]
    except KeyError as exc:
        raise TaskCenterInvariantViolation(
            f"Role {role!r} has no AttemptFailReason mapping"
        ) from exc


# ---- LaunchBuilder (role-parametrized AgentLaunch factory) -----------------


PLANNER_AGENT_NAME = "planner"
EVALUATOR_AGENT_NAME = "evaluator"


@dataclass(frozen=True, slots=True)
class LaunchBuilder:
    """Build :class:`AgentLaunch` records for each harness role."""

    runtime: LaunchCtx

    def for_planner(self, *, attempt: Attempt, task_id: str) -> AgentLaunch:
        episode = self._require_episode(attempt)
        return self._build(
            role=TaskCenterTaskRole.PLANNER,
            base_agent_name=PLANNER_AGENT_NAME,
            scope=ContextScope.for_planner(
                mission_id=episode.mission_id,
                episode_id=episode.id,
                attempt_id=attempt.id,
            ),
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            needs=(),
            mission_id=episode.mission_id,
        )

    def for_generator(
        self,
        *,
        attempt: Attempt,
        task: dict[str, Any],
        base_agent_name: str,
    ) -> AgentLaunch:
        episode = self._require_episode(attempt)
        task_id = str(task["id"])
        return self._build(
            role=TaskCenterTaskRole.GENERATOR,
            base_agent_name=base_agent_name,
            scope=ContextScope.for_generator(
                mission_id=episode.mission_id,
                episode_id=episode.id,
                attempt_id=attempt.id,
                task_id=task_id,
            ),
            task_id=task_id,
            task_center_run_id=task["task_center_run_id"],
            attempt_id=attempt.id,
            needs=tuple(task["needs"]),
            mission_id=episode.mission_id,
        )

    def for_evaluator(self, *, attempt: Attempt, task_id: str) -> AgentLaunch:
        episode = self._require_episode(attempt)
        return self._build(
            role=TaskCenterTaskRole.EVALUATOR,
            base_agent_name=EVALUATOR_AGENT_NAME,
            scope=ContextScope.for_evaluator(
                mission_id=episode.mission_id,
                episode_id=episode.id,
                attempt_id=attempt.id,
            ),
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            needs=tuple(attempt.generator_task_ids),
            mission_id=episode.mission_id,
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
            mission_id=mission_id,
        )

    def _require_episode(self, attempt: Attempt) -> Any:
        episode = self.runtime.episode_store.get(attempt.episode_id)
        if episode is None:
            raise TaskCenterInvariantViolation(
                f"Episode {attempt.episode_id!r} not found"
            )
        return episode
