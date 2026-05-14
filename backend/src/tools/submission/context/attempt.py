"""TaskCenter attempt-bound submission context resolution."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from task_center.api import (
    Attempt,
    AttemptOrchestrator,
    AttemptDeps,
    Episode,
    Mission,
    TaskCenterInvariantViolation,
)
from tools._framework.core.context import ToolExecutionContextService


class AttemptSubmissionContextError(RuntimeError):
    """User-facing submission context resolution failure."""


@dataclass(frozen=True, slots=True)
class AttemptSubmissionContext:
    """Attempt-bound submission context.

    Resolved when the executor task is attached to an Attempt. Tools
    that strictly require attempt context (e.g. ``submit_evaluation``) keep
    using this resolver.
    """

    task_center_task_id: str
    task: dict[str, Any]
    attempt: Attempt
    episode: Episode
    mission: Mission
    runtime: AttemptDeps
    orchestrator: AttemptOrchestrator


def resolve_attempt_submission_context(
    context: ToolExecutionContextService,
) -> AttemptSubmissionContext:
    """Resolve the current TaskCenter task into durable harness attempt context.

    Strict attempt mode — raises :class:`AttemptSubmissionContextError` if the
    task is not attached to an Attempt. Use this resolver from tools
    that genuinely require an attempt (planner submissions, evaluator
    submissions).
    """
    runtime, task, task_id = _resolve_runtime_task(context)
    return _resolve_attempt_context(
        runtime=runtime, task=task, task_id=task_id, context=context
    )


def _resolve_runtime_task(
    context: ToolExecutionContextService,
) -> tuple[AttemptDeps, dict[str, Any], str]:
    """Shared prelude: pull runtime + task row + task id from tool context."""
    runtime = context.get("attempt_runtime")
    if not isinstance(runtime, AttemptDeps):
        raise AttemptSubmissionContextError(
            "Missing harness attempt runtime for this TaskCenter submission."
        )

    task_id = str(context.get("task_center_task_id") or "")
    if not task_id or task_id.isspace():
        raise AttemptSubmissionContextError(
            "Missing TaskCenter task id for this submission."
        )

    task = runtime.task_store.get_task(task_id)
    if task is None:
        raise AttemptSubmissionContextError(
            f"TaskCenter task {task_id!r} was not found."
        )
    return runtime, task, task_id


def _resolve_attempt_context(
    *,
    runtime: AttemptDeps,
    task: dict[str, Any],
    task_id: str,
    context: ToolExecutionContextService,
) -> AttemptSubmissionContext:
    """Build :class:`AttemptSubmissionContext` from an already-fetched task.

    Shared between :func:`resolve_attempt_submission_context` and the
    attempt-mode branch of :func:`resolve_executor_submission_context` so the
    task row is fetched exactly once per call.
    """
    attempt_id = str(task.get("task_center_attempt_id") or "")
    if not attempt_id or attempt_id.isspace():
        raise AttemptSubmissionContextError(
            f"TaskCenter task {task_id!r} is not attached to a harness attempt."
        )

    metadata_attempt_id = str(context.get("task_center_attempt_id") or "")
    if metadata_attempt_id.isspace():
        raise AttemptSubmissionContextError(
            "TaskCenter attempt metadata is blank."
        )
    if metadata_attempt_id and metadata_attempt_id != attempt_id:
        raise AttemptSubmissionContextError(
            "TaskCenter attempt metadata does not match the persisted task row."
        )

    attempt = runtime.attempt_store.get(attempt_id)
    if attempt is None:
        raise AttemptSubmissionContextError(
            f"Attempt {attempt_id!r} was not found."
        )

    episode = runtime.episode_store.get(attempt.episode_id)
    if episode is None:
        raise AttemptSubmissionContextError(
            f"Episode {attempt.episode_id!r} was not found."
        )

    mission = runtime.mission_store.get(episode.mission_id)
    if mission is None:
        raise AttemptSubmissionContextError(
            f"Mission {episode.mission_id!r} was not found."
        )

    try:
        orchestrator = runtime.orchestrator_registry.get_or_raise(attempt_id)
    except TaskCenterInvariantViolation as exc:
        raise AttemptSubmissionContextError(str(exc)) from exc

    return AttemptSubmissionContext(
        task_center_task_id=task_id,
        task=task,
        attempt=attempt,
        episode=episode,
        mission=mission,
        runtime=runtime,
        orchestrator=orchestrator,
    )
