"""TaskCenter executor submission context resolution."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from tools._framework.core.context import ToolExecutionContextService
from tools.submission.context.attempt import (
    AttemptSubmissionContext,
    AttemptSubmissionContextError,
    _resolve_attempt_context,
    _resolve_runtime_task,
)

if TYPE_CHECKING:
    from task_center import AttemptDeps, StartedWorkflow


@dataclass(frozen=True, slots=True)
class ExecutorSubmissionContext:
    """Unified context for executor-shaped terminal submissions.

    Tools call :meth:`submit_executor_success`,
    :meth:`submit_executor_blocker`, or :meth:`start_delegated_workflow`
    for attempt-bound generator tasks.
    """

    task_center_task_id: str
    task: dict[str, Any]
    runtime: AttemptDeps
    attempt_ctx: AttemptSubmissionContext

    @property
    def attempt_id(self) -> str:
        return self.attempt_ctx.attempt.id

    def submit_executor_success(
        self, *, summary: str, artifacts: list[str]
    ) -> None:
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

    def submit_executor_blocker(self, *, summary: str) -> None:
        from task_center import GeneratorSubmission

        self.attempt_ctx.orchestrator.apply_generator_submission(
            GeneratorSubmission(
                attempt_id=self.attempt_ctx.attempt.id,
                task_id=self.task_center_task_id,
                outcome="blocker",
                summary=summary,
                payload={
                    "generator_role": "executor",
                },
            )
        )

    def start_delegated_workflow(
        self, *, goal_handoff: str
    ) -> StartedWorkflow:
        from task_center import WorkflowOrigin, WorkflowStarter

        coordinator = WorkflowStarter(runtime=self.runtime)
        return coordinator.start(
            prompt=goal_handoff,
            origin=WorkflowOrigin.task(task_id=self.task_center_task_id),
        )


def resolve_executor_submission_context(
    context: ToolExecutionContextService,
) -> ExecutorSubmissionContext:
    """Resolve a unified executor submission context.

    Executor terminal tools are valid only for attempt-bound generator tasks.
    """
    runtime, task, task_id = _resolve_runtime_task(context)
    attempt_id = str(task.get("task_center_attempt_id") or "")
    if not attempt_id or attempt_id.isspace():
        raise AttemptSubmissionContextError(
            f"TaskCenter task {task_id!r} is not attempt-bound; executor "
            "terminal submissions require a generator task."
        )
    attempt_ctx = _resolve_attempt_context(
        runtime=runtime, task=task, task_id=task_id, context=context
    )
    return ExecutorSubmissionContext(
        task_center_task_id=task_id,
        task=task,
        runtime=runtime,
        attempt_ctx=attempt_ctx,
    )


__all__ = [
    "ExecutorSubmissionContext",
    "resolve_executor_submission_context",
]
