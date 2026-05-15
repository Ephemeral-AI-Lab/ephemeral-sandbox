"""TaskCenter executor submission context resolution."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from tools._framework.core.context import ToolExecutionContextService
from tools.submission.context.trial import (
    TrialSubmissionContext,
    TrialSubmissionContextError,
    _resolve_trial_context,
    _resolve_runtime_task,
)

if TYPE_CHECKING:
    from task_center import TrialDeps, EntryTaskController, StartedGoal


@dataclass(frozen=True, slots=True)
class ExecutorSubmissionContext:
    """Unified context for executor-shaped terminal submissions.

    Tools call :meth:`submit_executor_success`,
    :meth:`submit_executor_failure`, or :meth:`start_delegated_goal`
    without knowing whether the task is trial-bound or entry-mode. The
    context dispatches to the right backend (orchestrator vs entry
    controller) internally.

    Exactly one of ``attempt_ctx`` and ``entry_controller`` is set.
    """

    task_center_task_id: str
    task: dict[str, Any]
    runtime: TrialDeps
    attempt_ctx: TrialSubmissionContext | None
    entry_controller: EntryTaskController | None

    @property
    def attempt_id(self) -> str | None:
        return self.attempt_ctx.attempt.id if self.attempt_ctx is not None else None

    def submit_executor_success(
        self, *, summary: str, artifacts: list[str]
    ) -> None:
        if self.attempt_ctx is not None:
            from task_center import GeneratorSubmission

            self.attempt_ctx.orchestrator.apply_generator_submission(
                GeneratorSubmission(
                    attempt_id=self.attempt_ctx.attempt.id,
                    task_id=self.task_center_task_id,
                    outcome="success",
                    summary=summary,
                    payload={
                        "generator_role": "executor",
                        "artifacts": artifacts,
                    },
                )
            )
            return
        assert self.entry_controller is not None
        self.entry_controller.apply_executor_success(
            summary=summary, artifacts=artifacts
        )

    def submit_executor_failure(
        self, *, summary: str, reason: str, details: list[str]
    ) -> None:
        if self.attempt_ctx is not None:
            from task_center import GeneratorSubmission

            self.attempt_ctx.orchestrator.apply_generator_submission(
                GeneratorSubmission(
                    attempt_id=self.attempt_ctx.attempt.id,
                    task_id=self.task_center_task_id,
                    outcome="failure",
                    summary=summary,
                    payload={
                        "generator_role": "executor",
                        "reason": reason,
                        "details": details,
                    },
                )
            )
            return
        assert self.entry_controller is not None
        self.entry_controller.apply_executor_failure(
            summary=summary, reason=reason, details=details
        )

    def start_delegated_goal(
        self, *, goal: str
    ) -> StartedGoal:
        from task_center import GoalStarter

        coordinator = GoalStarter(runtime=self.runtime)
        return coordinator.start(
            parent_task_id=self.task_center_task_id,
            goal=goal,
        )


def resolve_executor_submission_context(
    context: ToolExecutionContextService,
) -> ExecutorSubmissionContext:
    """Resolve a unified executor submission context.

    Branches on whether the task row's ``task_center_attempt_id`` is
    set (attempt mode) or ``None`` (entry mode). Tools that accept either
    shape call this resolver and use the resulting
    :class:`ExecutorSubmissionContext` operations.
    """
    runtime, task, task_id = _resolve_runtime_task(context)
    attempt_id = str(task.get("task_center_attempt_id") or "")
    if attempt_id and not attempt_id.isspace():
        attempt_ctx = _resolve_trial_context(
            runtime=runtime, task=task, task_id=task_id, context=context
        )
        return ExecutorSubmissionContext(
            task_center_task_id=task_id,
            task=task,
            runtime=runtime,
            attempt_ctx=attempt_ctx,
            entry_controller=None,
        )

    controller = runtime.entry_task_controller
    if controller is None or controller.task_id != task_id:
        raise TrialSubmissionContextError(
            f"TaskCenter task {task_id!r} is entry-mode but no entry "
            "controller is bound to it; the spawn was set up incorrectly."
        )
    return ExecutorSubmissionContext(
        task_center_task_id=task_id,
        task=task,
        runtime=runtime,
        attempt_ctx=None,
        entry_controller=controller,
    )


__all__ = [
    "ExecutorSubmissionContext",
    "resolve_executor_submission_context",
]
