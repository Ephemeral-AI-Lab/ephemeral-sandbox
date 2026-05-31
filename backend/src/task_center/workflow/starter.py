"""WorkflowStarter — single safe path from prompt text to Workflow execution.

Owns parent-task validation, the atomic ``RUNNING -> WAITING_WORKFLOW`` flip +
``child_workflow_id`` link, iteration/attempt startup, and compensation
(including the M1 orphan-guard) on failure. The parent task is either an
attempt-bound generator (a ``submit_workflow_handoff`` child) or the synthetic
run-level bootstrap generator ``<run_id>:root`` (the root workflow).
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from task_center._core.outcomes import execution_outcome_for_submission, to_record
from task_center._core.primitives import (
    TaskCenterInvariantViolation,
    attempt_id_from_task_id,
)
from task_center._core.state import (
    AttemptFailReason,
    AttemptStatus,
    IterationStatus,
    Workflow,
    WorkflowStatus,
)
from task_center._core.task_state import TaskCenterTaskStatus
from task_center.attempt.launch import AttemptDeps
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.iteration import OrchestratorFactory
from task_center.workflow.lifecycle import RunCloseHandler, WorkflowLifecycle

logger = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class StartedWorkflow:
    parent_task_id: str
    parent_attempt_id: str | None
    workflow_id: str
    iteration_id: str
    attempt_id: str


def _no_root_close_handler(*, child_workflow: Workflow) -> None:
    raise TaskCenterInvariantViolation(
        f"Root workflow {child_workflow.id!r} closed without a run-close handler."
    )


class WorkflowStarter:
    """Single orchestration entry point for prompt → workflow start."""

    def __init__(
        self,
        *,
        runtime: AttemptDeps,
        run_close_handler: RunCloseHandler | None = None,
        orchestrator_factory: OrchestratorFactory | None = None,
    ) -> None:
        self._runtime = runtime
        self._run_close_handler = run_close_handler or _no_root_close_handler
        self._orchestrator_factory = orchestrator_factory or (
            lambda attempt, on_attempt_closed: AttemptOrchestrator(
                attempt=attempt,
                on_attempt_closed=on_attempt_closed,
                runtime=self._runtime,
            )
        )

    def start(self, *, prompt: str, parent_task_id: str) -> StartedWorkflow:
        prompt = prompt.strip()
        if not prompt:
            raise TaskCenterInvariantViolation("Workflow prompt must be nonblank.")
        parent_task = self._assert_parent_running_and_no_open_child(parent_task_id)
        run_id = str(parent_task.get("task_center_run_id") or "")
        if not run_id.strip():
            raise TaskCenterInvariantViolation(f"Parent task {parent_task_id!r} has no run id.")
        parent_attempt_id = attempt_id_from_task_id(parent_task_id)

        lifecycle = self._build_workflow_lifecycle()
        workflow = lifecycle.create_workflow(
            task_center_run_id=run_id,
            parent_task_id=parent_task_id,
            workflow_goal=prompt,
        )
        iteration, iteration_coordinator = lifecycle.create_iteration_with_coordinator(
            workflow_id=workflow.id,
        )

        def _before_start(attempt: Any) -> None:
            self._mark_parent_waiting(
                parent_task=parent_task,
                parent_attempt_id=parent_attempt_id,
                workflow=workflow,
            )

        try:
            attempt = iteration_coordinator.create_and_start_first_attempt(
                before_start=_before_start
            )
        except Exception:
            refreshed = self._runtime.iteration_store.get(iteration.id)
            attempt_id = refreshed.latest_attempt_id if refreshed else None
            self._compensate_failed_start(
                workflow=workflow,
                iteration_id=iteration.id,
                attempt_id=attempt_id,
                parent_task=parent_task,
                parent_attempt_id=parent_attempt_id,
            )
            raise

        return StartedWorkflow(
            parent_task_id=parent_task_id,
            parent_attempt_id=parent_attempt_id,
            workflow_id=workflow.id,
            iteration_id=iteration.id,
            attempt_id=attempt.id,
        )

    def _build_workflow_lifecycle(self) -> WorkflowLifecycle:
        iteration_coordinators = self._runtime.iteration_coordinators
        if iteration_coordinators is None:
            raise TaskCenterInvariantViolation("WorkflowStarter requires open iteration coordinators.")
        return WorkflowLifecycle(
            workflow_store=self._runtime.workflow_store,
            iteration_store=self._runtime.iteration_store,
            attempt_store=self._runtime.attempt_store,
            iteration_coordinators=iteration_coordinators,
            config=self._runtime.lifecycle_config,
            orchestrator_registry=self._runtime.orchestrator_registry,
            run_close_handler=self._run_close_handler,
            orchestrator_factory=self._orchestrator_factory,
            task_store=self._runtime.task_store,
        )

    def _assert_parent_running_and_no_open_child(self, parent_task_id: str) -> dict[str, Any]:
        task = self._runtime.task_store.get_task(parent_task_id)
        if task is None:
            raise TaskCenterInvariantViolation(f"TaskCenter task {parent_task_id!r} was not found.")
        if task.get("status") != TaskCenterTaskStatus.RUNNING.value:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} is not running; "
                "delegated workflow start requires a running parent task."
            )
        open_workflows = [
            r for r in self._runtime.workflow_store.list_for_parent_task(parent_task_id) if r.is_open
        ]
        if open_workflows:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {parent_task_id!r} already has an open "
                f"delegated workflow {open_workflows[0].id!r}."
            )
        return task

    def _mark_parent_waiting(
        self,
        *,
        parent_task: dict[str, Any],
        parent_attempt_id: str | None,
        workflow: Workflow,
    ) -> None:
        if parent_attempt_id is not None:
            orchestrator = self._runtime.orchestrator_registry.get(parent_attempt_id)
            if orchestrator is None:
                raise TaskCenterInvariantViolation(
                    f"Parent AttemptOrchestrator for attempt {parent_attempt_id!r} is not "
                    "registered; workflow start cannot proceed."
                )
            orchestrator.start_child_workflow(generator_task=parent_task, child_workflow=workflow)
            return
        # Root: the synthetic bootstrap task has no orchestrator — flip it directly.
        updated = self._runtime.task_store.set_task_status_if_current(
            str(parent_task["task_id"]),
            expected_status=TaskCenterTaskStatus.RUNNING.value,
            status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
            child_workflow_id=workflow.id,
        )
        if updated is None:
            raise TaskCenterInvariantViolation(
                f"Root bootstrap task {parent_task['task_id']!r} was not running when "
                "the root workflow start tried to mark it waiting."
            )

    def _compensate_failed_start(
        self,
        *,
        workflow: Workflow,
        iteration_id: str,
        attempt_id: str | None,
        parent_task: dict[str, Any],
        parent_attempt_id: str | None,
    ) -> None:
        """Best-effort rollback: attempt -> iteration -> workflow -> parent (M1)."""
        now = datetime.now(UTC)
        runtime = self._runtime

        def _do(step_name: str, action: Callable[[], object]) -> bool:
            try:
                action()
                return True
            except Exception:
                logger.exception("WorkflowStart compensation step %r failed", step_name)
                return False

        _do("close_unstarted_attempt", lambda: self._close_unstarted_attempt(attempt_id, now=now))
        _do(
            "cancel_iteration",
            lambda: runtime.iteration_store.set_status(
                iteration_id, status=IterationStatus.CANCELLED, closed_at=now
            ),
        )
        _do(
            "cancel_workflow",
            lambda: runtime.workflow_store.set_status(
                workflow.id, status=WorkflowStatus.CANCELLED, closed_at=now
            ),
        )
        self._restore_or_fail_parent(
            parent_task=parent_task, parent_attempt_id=parent_attempt_id, do=_do
        )
        if runtime.iteration_coordinators is not None:
            runtime.iteration_coordinators.deregister(iteration_id)

    def _restore_or_fail_parent(
        self,
        *,
        parent_task: dict[str, Any],
        parent_attempt_id: str | None,
        do: Callable[[str, Callable[[], object]], bool],
    ) -> None:
        task_id = str(parent_task["task_id"])
        if parent_attempt_id is None:
            # Root bootstrap: restore RUNNING; the run controller's seed
            # failsafe then finishes the run.
            do(
                "restore_root",
                lambda: self._runtime.task_store.set_task_status_if_current(
                    task_id,
                    expected_status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
                    status=TaskCenterTaskStatus.RUNNING.value,
                ),
            )
            return
        orchestrator = self._runtime.orchestrator_registry.get(parent_attempt_id)
        restored = False
        if orchestrator is not None:
            restored = do(
                "cancel_child_workflow",
                lambda: orchestrator.cancel_child_workflow(generator_task=parent_task),
            )
        if not restored:
            # M1 orphan-guard last resort: a WAITING_WORKFLOW generator can never
            # be stranded — force it FAILED.
            do(
                "orphan_guard_fail",
                lambda: self._runtime.task_store.set_task_status_if_current(
                    task_id,
                    expected_status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
                    status=TaskCenterTaskStatus.FAILED.value,
                    outcomes=[
                        to_record(
                            execution_outcome_for_submission(
                                task_id=task_id,
                                role="generator",
                                status="failed",
                                outcome="Child workflow start failed.",
                            )
                        )
                    ],
                    terminal_tool_result={"fail_reason": "workflow_start_failed"},
                ),
            )

    def _close_unstarted_attempt(self, attempt_id: str | None, *, now: datetime) -> None:
        if attempt_id is None:
            return
        attempt = self._runtime.attempt_store.get(attempt_id)
        if attempt is None or attempt.is_closed:
            return
        self._runtime.attempt_store.close(
            attempt_id,
            status=AttemptStatus.FAILED,
            fail_reason=AttemptFailReason.STARTUP_FAILED,
            closed_at=now,
        )
