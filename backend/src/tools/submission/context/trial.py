"""TaskCenter trial-bound submission context resolution."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from task_center import (
    Trial,
    TrialOrchestrator,
    TrialDeps,
    Iteration,
    Goal,
    TaskCenterInvariantViolation,
)
from tools._framework.core.context import ToolExecutionContextService


class TrialSubmissionContextError(RuntimeError):
    """User-facing submission context resolution failure."""


@dataclass(frozen=True, slots=True)
class TrialSubmissionContext:
    """Trial-bound submission context.

    Resolved when the executor task is attached to a Trial. Tools
    that strictly require trial context (e.g. ``submit_evaluation``) keep
    using this resolver.
    """

    task_center_task_id: str
    task: dict[str, Any]
    attempt: Trial
    episode: Iteration
    mission: Goal
    runtime: TrialDeps
    orchestrator: TrialOrchestrator


def resolve_trial_submission_context(
    context: ToolExecutionContextService,
) -> TrialSubmissionContext:
    """Resolve the current TaskCenter task into durable harness trial context.

    Strict trial mode — raises :class:`TrialSubmissionContextError` if the
    task is not attached to a Trial. Use this resolver from tools
    that genuinely require a trial (planner submissions, evaluator
    submissions).
    """
    runtime, task, task_id = _resolve_runtime_task(context)
    return _resolve_trial_context(
        runtime=runtime, task=task, task_id=task_id, context=context
    )


def _resolve_runtime_task(
    context: ToolExecutionContextService,
) -> tuple[TrialDeps, dict[str, Any], str]:
    """Shared prelude: pull runtime + task row + task id from tool context."""
    runtime = context.get("attempt_runtime")
    if not isinstance(runtime, TrialDeps):
        raise TrialSubmissionContextError(
            "Missing harness attempt runtime for this TaskCenter submission."
        )

    task_id = str(context.get("task_center_task_id") or "")
    if not task_id or task_id.isspace():
        raise TrialSubmissionContextError(
            "Missing TaskCenter task id for this submission."
        )

    task = runtime.task_store.get_task(task_id)
    if task is None:
        raise TrialSubmissionContextError(
            f"TaskCenter task {task_id!r} was not found."
        )
    return runtime, task, task_id


def _resolve_trial_context(
    *,
    runtime: TrialDeps,
    task: dict[str, Any],
    task_id: str,
    context: ToolExecutionContextService,
) -> TrialSubmissionContext:
    """Build :class:`TrialSubmissionContext` from an already-fetched task.

    Shared between :func:`resolve_trial_submission_context` and the
    trial-mode branch of :func:`resolve_executor_submission_context` so the
    task row is fetched exactly once per call.
    """
    attempt_id = str(task.get("task_center_attempt_id") or "")
    if not attempt_id or attempt_id.isspace():
        raise TrialSubmissionContextError(
            f"TaskCenter task {task_id!r} is not attached to a harness attempt."
        )

    metadata_attempt_id = str(context.get("task_center_attempt_id") or "")
    if metadata_attempt_id.isspace():
        raise TrialSubmissionContextError(
            "TaskCenter attempt metadata is blank."
        )
    if metadata_attempt_id and metadata_attempt_id != attempt_id:
        raise TrialSubmissionContextError(
            "TaskCenter attempt metadata does not match the persisted task row."
        )

    attempt = runtime.trial_store.get(attempt_id)
    if attempt is None:
        raise TrialSubmissionContextError(
            f"Attempt {attempt_id!r} was not found."
        )

    episode = runtime.iteration_store.get(attempt.iteration_id)
    if episode is None:
        raise TrialSubmissionContextError(
            f"Iteration {attempt.iteration_id!r} was not found."
        )

    mission = runtime.goal_store.get(episode.goal_id)
    if mission is None:
        raise TrialSubmissionContextError(
            f"Goal {episode.goal_id!r} was not found."
        )

    try:
        orchestrator = runtime.orchestrator_registry.get_or_raise(attempt_id)
    except TaskCenterInvariantViolation as exc:
        raise TrialSubmissionContextError(str(exc)) from exc

    return TrialSubmissionContext(
        task_center_task_id=task_id,
        task=task,
        attempt=attempt,
        episode=episode,
        mission=mission,
        runtime=runtime,
        orchestrator=orchestrator,
    )
