"""AttemptDispatcher — DAG dispatch helper for AttemptOrchestrator.

Owns the launch/quiescence state machine for one attempt's generators and
evaluator. Calls back into the orchestrator's ``_close_attempt`` for the actual
attempt-closing transition; the orchestrator remains the only owner of
close-attempt state and the on_attempt_closed signal to ``EpisodeManager``.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any

from task_center._core.infra import TaskCenterAuditEmitter
from task_center._core.types import TaskCenterInvariantViolation
from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.context_engine.scope import ContextScope
from task_center.attempt.runtime import (
    AgentLaunch,
    AttemptDeps,
)
from task_center.attempt.launch import LaunchBuilder
from task_center.attempt.generator_dag import (
    all_generators_done,
    all_generators_quiescent,
    any_generator_failed_or_blocked,
    blocked_descendant_ids,
    ready_pending_generator_ids,
)
from task_center._core.types import evaluator_task_id
from task_center.task_state import (
    SpawnReason,
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
)

logger = logging.getLogger(__name__)


CloseGraphCallback = Callable[
    [AttemptStatus, AttemptFailReason | None], None
]


# Stage → dispatch-method name. PLAN and CLOSED stages are no-ops.
_STAGE_DISPATCH: dict[AttemptStage, str] = {
    AttemptStage.GENERATE: "_dispatch_generating",
    AttemptStage.EVALUATE: "_dispatch_evaluating",
}


class AttemptDispatcher:
    """Drives the generator-DAG and evaluator launch/quiescence machine."""

    def __init__(
        self,
        *,
        attempt_id: str,
        runtime: AttemptDeps,
        close_attempt: CloseGraphCallback,
    ) -> None:
        self._attempt_id = attempt_id
        self._runtime = runtime
        self._close_attempt = close_attempt
        self._audit = TaskCenterAuditEmitter(runtime.audit_sink)

    # ---- public API -----------------------------------------------------

    def dispatch_ready_work(self) -> None:
        attempt = self._fresh_attempt()
        if attempt.is_closed:
            return
        method = _STAGE_DISPATCH.get(attempt.stage)
        if method is not None:
            getattr(self, method)(attempt)

    def block_failed_descendants(self, failed_task_id: str) -> None:
        runtime = self._runtime
        attempt = self._fresh_attempt()
        task_records = runtime.task_store.list_generator_tasks_for_attempt(
            attempt.id
        )
        for task_id in blocked_descendant_ids(
            failed_task_id=failed_task_id,
            task_records=task_records,
        ):
            runtime.task_store.set_task_status(
                task_id,
                status=TaskCenterTaskStatus.BLOCKED.value,
                summary={"blocked_by": failed_task_id},
            )

    # ---- internal -------------------------------------------------------

    def _dispatch_generating(self, attempt: Attempt) -> None:
        runtime = self._runtime
        task_records = runtime.task_store.list_generator_tasks_for_attempt(
            attempt.id
        )
        ready_ids = ready_pending_generator_ids(task_records)
        if ready_ids:
            launch_failed = False
            for task_id in ready_ids:
                if not self._launch_ready_generator(
                    attempt=attempt,
                    task_id=task_id,
                ):
                    launch_failed = True
            if launch_failed:
                self.dispatch_ready_work()
            return

        if not all_generators_quiescent(task_records):
            return

        if any_generator_failed_or_blocked(task_records):
            self._close_attempt(
                AttemptStatus.FAILED,
                AttemptFailReason.GENERATOR_FAILED,
            )
            return

        if all_generators_done(task_records):
            self._spawn_evaluator(attempt)

    def _dispatch_evaluating(self, attempt: Attempt) -> None:
        if attempt.evaluator_task_id is None:
            raise TaskCenterInvariantViolation(
                f"Attempt {attempt.id!r} is evaluating with no evaluator task"
            )
        runtime = self._runtime
        evaluator_task = runtime.task_store.get_task(attempt.evaluator_task_id)
        if evaluator_task is None:
            raise TaskCenterInvariantViolation(
                f"Evaluator task {attempt.evaluator_task_id!r} not found"
            )
        status = TaskCenterTaskStatus(evaluator_task["status"])
        if status == TaskCenterTaskStatus.DONE:
            self._close_attempt(AttemptStatus.PASSED, None)
        elif status == TaskCenterTaskStatus.FAILED:
            self._close_attempt(
                AttemptStatus.FAILED,
                AttemptFailReason.EVALUATOR_FAILED,
            )

    def _launch_ready_generator(
        self, *, attempt: Attempt, task_id: str
    ) -> bool:
        runtime = self._runtime
        current = runtime.task_store.get_task(task_id)
        if current is None:
            raise TaskCenterInvariantViolation(f"Generator task {task_id!r} not found")
        agent_name = self._task_agent_name(current)
        self._audit.task_ready(
            current,
            attempt_id=attempt.id,
            satisfied_dependency_ids=tuple(
                str(dep) for dep in current.get("needs", ()) or ()
            ),
        )
        task = runtime.task_store.set_task_status(
            task_id, status=TaskCenterTaskStatus.RUNNING.value
        )
        self._audit.task_launched(task, attempt_id=attempt.id)
        try:
            launch = LaunchBuilder(runtime=runtime).for_generator(
                attempt=attempt,
                task=task,
                base_agent_name=agent_name,
            )
            if launch.context_packet_id is not None:
                runtime.task_store.set_task_context_packet_id(
                    task_id,
                    context_packet_id=launch.context_packet_id,
                )
            runtime.agent_launcher.launch(launch)
        except Exception:
            logger.exception(
                "AttemptDispatcher: generator launch failed",
                extra={"task_id": task_id, "attempt_id": attempt.id},
            )
            runtime.task_store.set_task_status_if_current(
                task_id,
                expected_status=TaskCenterTaskStatus.RUNNING.value,
                status=TaskCenterTaskStatus.FAILED.value,
                summary={
                    "fail_reason": "agent_launch_failed",
                    "summary": "Generator agent launch failed.",
                },
            )
            failed_task = runtime.task_store.get_task(task_id) or task
            self._audit.task_failed(
                failed_task,
                attempt_id=attempt.id,
                fail_reason="agent_launch_failed",
                summary="Generator agent launch failed.",
            )
            self.block_failed_descendants(task_id)
            return False
        return True

    def _launch_evaluator(self, launch: AgentLaunch) -> None:
        runtime = self._runtime
        try:
            runtime.agent_launcher.launch(launch)
        except Exception:
            logger.exception(
                "AttemptDispatcher: evaluator launch failed",
                extra={
                    "task_id": launch.task_id,
                    "attempt_id": launch.attempt_id,
                },
            )
            runtime.task_store.set_task_status_if_current(
                launch.task_id,
                expected_status=TaskCenterTaskStatus.RUNNING.value,
                status=TaskCenterTaskStatus.FAILED.value,
                summary={
                    "fail_reason": "agent_launch_failed",
                    "summary": "Evaluator agent launch failed.",
                },
            )
            failed_task = runtime.task_store.get_task(launch.task_id)
            if failed_task is not None:
                self._audit.task_failed(
                    failed_task,
                    attempt_id=launch.attempt_id,
                    fail_reason="agent_launch_failed",
                    summary="Evaluator agent launch failed.",
                )
            self._close_attempt(
                AttemptStatus.FAILED,
                AttemptFailReason.EVALUATOR_FAILED,
            )

    def _spawn_evaluator(self, attempt: Attempt) -> None:
        if attempt.evaluator_task_id is not None:
            return
        runtime = self._runtime
        task_id = evaluator_task_id(attempt.id)
        try:
            launch = LaunchBuilder(runtime=runtime).for_evaluator(
                attempt=attempt, task_id=task_id
            )
            ready_task = {
                "id": task_id,
                "task_center_run_id": launch.task_center_run_id,
                "role": TaskCenterTaskRole.EVALUATOR.value,
                "agent_name": launch.agent_name,
                "status": TaskCenterTaskStatus.PENDING.value,
                "needs": list(attempt.generator_task_ids),
                "task_center_attempt_id": attempt.id,
                "context_packet_id": launch.context_packet_id,
            }
            self._audit.task_ready(
                ready_task,
                attempt_id=attempt.id,
                satisfied_dependency_ids=tuple(attempt.generator_task_ids),
            )
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=launch.task_center_run_id,
                role=TaskCenterTaskRole.EVALUATOR.value,
                agent_name=launch.agent_name,
                rendered_prompt=launch.rendered_prompt,
                status=TaskCenterTaskStatus.RUNNING.value,
                summaries=[],
                needs=list(attempt.generator_task_ids),
                task_center_attempt_id=attempt.id,
                context_packet_id=launch.context_packet_id,
                spawn_reason=SpawnReason.ATTEMPT_EVALUATOR.value,
            )
            task = runtime.task_store.get_task(task_id)
            if task is not None:
                self._audit.task_launched(task, attempt_id=attempt.id)
            runtime.attempt_store.set_evaluator_task_id(attempt.id, task_id)
            runtime.attempt_store.set_stage(attempt.id, AttemptStage.EVALUATE)
            self._launch_evaluator(launch)
        except Exception:
            logger.exception(
                "AttemptDispatcher: evaluator spawn failed",
                extra={"task_id": task_id, "attempt_id": attempt.id},
            )
            self._fail_evaluator_spawn(task_id)
            raise

    def _fail_evaluator_spawn(self, task_id: str) -> None:
        try:
            self._runtime.task_store.set_task_status_if_current(
                task_id,
                expected_status=TaskCenterTaskStatus.RUNNING.value,
                status=TaskCenterTaskStatus.FAILED.value,
                summary={
                    "fail_reason": "agent_launch_failed",
                    "summary": "Evaluator agent startup failed.",
                },
            )
        except LookupError:
            pass
        self._close_attempt(
            AttemptStatus.FAILED,
            AttemptFailReason.EVALUATOR_FAILED,
        )

    @staticmethod
    def _task_agent_name(task: dict[str, Any]) -> str:
        agent_name = str(task.get("agent_name") or "").strip()
        if not agent_name:
            raise TaskCenterInvariantViolation(
                f"Task {task.get('id')!r} has no persisted agent profile"
            )
        return agent_name

    def _fresh_attempt(self) -> Attempt:
        attempt = self._runtime.attempt_store.get(self._attempt_id)
        if attempt is None:
            raise TaskCenterInvariantViolation(
                f"Attempt {self._attempt_id!r} not found"
            )
        return attempt
