"""Production launcher + AgentLaunchFactory for TaskCenter harness agents."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

from agents import get_definition
from message.message import Message
from message.events import StreamEvent
from task_center.attempt.orchestrator_registry import RegisteredAttemptOrchestrator
from task_center.attempt.deps import AgentLaunch, AttemptDeps
from task_center.attempt.state import AttemptFailReason, AttemptStatus
from task_center.context_engine.scope import ContextScope
from task_center._core.primitives import TaskCenterInvariantViolation
from task_center._core.task_state import (
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
)
from task_center.submissions import (
    EvaluatorSubmission,
    GeneratorSubmission,
    PlannerFailureSubmission,
)
from tools import ExecutionMetadata

if TYPE_CHECKING:
    from agents import AgentDefinition
    from runtime.app_factory import RuntimeConfig
    from task_center.attempt.state import Attempt


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
        deps_provider: AttemptDepsProvider,
        sandbox_id: str | None = None,
        on_event: AgentStreamEmitter | None = None,
        runner: AttemptAgentRunner | None = None,
    ) -> None:
        self._config = config
        self._deps_provider = deps_provider
        self._sandbox_id = sandbox_id
        self._on_event = on_event
        self._runner = runner
        self._pending: set[asyncio.Task[None]] = set()

    def launch(self, launch: AgentLaunch) -> None:
        agent_def = launch.agent_def or get_definition(launch.agent_name)
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
        runtime = self._deps_provider()
        if runtime is None:
            raise TaskCenterInvariantViolation("TaskCenter attempt runtime is not initialized.")
        runner = self._runner
        if runner is None:
            from engine.api import run_ephemeral_agent

            runner = run_ephemeral_agent

        # Runtime is always attached: submission tools resolve through the
        # attempt id carried on normal planner/generator/evaluator launches.
        metadata = ExecutionMetadata(
            task_center_run_id=launch.task_center_run_id,
            task_center_task_id=launch.task_id,
            task_center_attempt_id=launch.attempt_id,
            task_center_workflow_id=launch.workflow_id,
            task_center_request_id=launch.workflow_id,
            attempt_runtime=runtime,
            composer=runtime.composer,
        )
        metadata["active_terminals"] = list(agent_def.terminals)
        # Canonical initial-message order: [system, context, guidance, skill?].
        # system is the agent_def system prompt; the remaining rows are user
        # messages and the last one becomes the runner's spawn prompt (the
        # runner appends it after initial_messages). A planner with a declared
        # skill yields 4 rows; the main-agent default (guidance, no skill) is 3.
        rows = [r for r in (launch.context, launch.task_guidance, launch.skill) if r]
        runner_initial_messages: list[Message] | None
        if rows:
            runner_prompt = rows[-1]
            runner_initial_messages = [Message.from_user_text(r) for r in rows[:-1]] or None
        else:
            runner_prompt = launch.context
            runner_initial_messages = None
        try:
            result: Any = await runner(
                self._config,
                runner_prompt,
                agent_def=agent_def,
                sandbox_id=self._sandbox_id,
                persist_agent_run=True,
                task_id=launch.task_id,
                on_event=self._on_event,
                extra_tool_metadata=metadata,
                initial_messages=runner_initial_messages,
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

    async def _report_unfinished_running_task(
        self,
        launch: AgentLaunch,
        *,
        summary: str,
    ) -> None:
        runtime = self._deps_provider()
        if runtime is None:
            return
        task = runtime.task_store.get_task(launch.task_id)
        if task is None or task.get("status") != TaskCenterTaskStatus.RUNNING.value:
            # The lifecycle owner has already moved the task off RUNNING.
            return

        _report_exhaustion(runtime, launch, summary=summary)


_ROLE_FAIL_REASONS: dict[TaskCenterTaskRole, AttemptFailReason] = {
    TaskCenterTaskRole.PLANNER: AttemptFailReason.PLANNER_FAILED,
    TaskCenterTaskRole.GENERATOR: AttemptFailReason.GENERATOR_FAILED,
    TaskCenterTaskRole.EVALUATOR: AttemptFailReason.EVALUATOR_FAILED,
}


def _fail_unowned_attempt(
    runtime: AttemptDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> None:
    """Close task + attempt directly when the orchestrator is missing."""
    logger.error(
        "EphemeralAttemptAgentLauncher: missing orchestrator for unfinished task",
        extra={"task_id": launch.task_id, "attempt_id": launch.attempt_id},
    )
    runtime.task_store.set_task_status(
        launch.task_id,
        status=TaskCenterTaskStatus.FAILED.value,
        summary={"fail_reason": "run_exhausted", "summary": summary},
    )
    attempt = runtime.attempt_store.get(launch.attempt_id)
    if attempt is None or attempt.is_closed:
        return
    runtime.attempt_store.close(
        attempt.id,
        status=AttemptStatus.FAILED,
        fail_reason=_ROLE_FAIL_REASONS[launch.role],
        closed_at=datetime.now(UTC),
    )
    iteration_coordinators = runtime.iteration_coordinators
    if iteration_coordinators is None:
        return
    coordinator = iteration_coordinators.get(attempt.iteration_id)
    if coordinator is not None:
        coordinator.handle_attempt_closed(attempt.id)


def _require_attempt_orchestrator(
    runtime: AttemptDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> RegisteredAttemptOrchestrator | None:
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
    runtime: AttemptDeps,
    launch: AgentLaunch,
    *,
    summary: str,
) -> None:
    """Single role-parameterized exhaustion reporter."""
    orchestrator = _require_attempt_orchestrator(runtime, launch, summary=summary)
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
        raise TaskCenterInvariantViolation(f"No exhaustion reporter for role {launch.role!r}")


# ---- AgentLaunchFactory (role-parametrized AgentLaunch factory) ------------


PLANNER_AGENT_NAME = "planner"
EVALUATOR_AGENT_NAME = "evaluator"


@dataclass(frozen=True, slots=True)
class AgentLaunchFactory:
    """Build :class:`AgentLaunch` records for each harness role."""

    runtime: AttemptDeps

    def for_planner(self, *, attempt: Attempt, task_id: str) -> AgentLaunch:
        iteration = self._require_iteration(attempt)
        return self._build(
            role=TaskCenterTaskRole.PLANNER,
            base_agent_name=PLANNER_AGENT_NAME,
            scope=ContextScope.for_planner(
                workflow_id=iteration.workflow_id,
                iteration_id=iteration.id,
                attempt_id=attempt.id,
            ),
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            needs=(),
            workflow_id=iteration.workflow_id,
        )

    def for_generator(
        self,
        *,
        attempt: Attempt,
        task: dict[str, Any],
        base_agent_name: str,
    ) -> AgentLaunch:
        iteration = self._require_iteration(attempt)
        task_id = str(task["id"])
        return self._build(
            role=TaskCenterTaskRole.GENERATOR,
            base_agent_name=base_agent_name,
            scope=ContextScope.for_generator(
                workflow_id=iteration.workflow_id,
                iteration_id=iteration.id,
                attempt_id=attempt.id,
                task_id=task_id,
            ),
            task_id=task_id,
            task_center_run_id=task["task_center_run_id"],
            attempt_id=attempt.id,
            needs=tuple(task["needs"]),
            workflow_id=iteration.workflow_id,
        )

    def for_evaluator(self, *, attempt: Attempt, task_id: str) -> AgentLaunch:
        iteration = self._require_iteration(attempt)
        return self._build(
            role=TaskCenterTaskRole.EVALUATOR,
            base_agent_name=EVALUATOR_AGENT_NAME,
            scope=ContextScope.for_evaluator(
                workflow_id=iteration.workflow_id,
                iteration_id=iteration.id,
                attempt_id=attempt.id,
            ),
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            needs=tuple(attempt.generator_task_ids),
            workflow_id=iteration.workflow_id,
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
        workflow_id: str | None,
    ) -> AgentLaunch:
        messages = self.runtime.require_composer().compose(
            base_agent_name=base_agent_name, scope=scope
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            attempt_id=attempt_id,
            role=role,
            agent_name=messages.agent_def.name,
            agent_def=messages.agent_def,
            context=messages.context,
            task_guidance=messages.task_guidance,
            needs=needs,
            context_packet_id=messages.context_packet_id,
            workflow_id=workflow_id,
            skill=messages.skill,
        )

    def _require_iteration(self, attempt: Attempt) -> Any:
        iteration = self.runtime.iteration_store.get(attempt.iteration_id)
        if iteration is None:
            raise TaskCenterInvariantViolation(f"Iteration {attempt.iteration_id!r} not found")
        return iteration
