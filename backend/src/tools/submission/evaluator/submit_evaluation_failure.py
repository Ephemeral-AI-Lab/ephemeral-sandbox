"""submit_evaluation_failure terminal tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from task_center import (
    EvaluatorSubmission,
    TaskCenterInvariantViolation,
)
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools.submission.context import (
    TrialSubmissionContextError,
    resolve_trial_submission_context,
)


class SubmitEvaluationFailureInput(BaseModel):
    summary: str = Field(..., min_length=1)
    failed_criteria: list[str] = Field(default_factory=list)


@tool(
    name="submit_evaluation_failure",
    description="Submit attempt-level evaluation failure.",
    input_model=SubmitEvaluationFailureInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
)
async def submit_evaluation_failure(
    summary: str,
    failed_criteria: list[str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_trial_submission_context(context)
        submission_context.orchestrator.apply_evaluator_submission(
            EvaluatorSubmission(
                attempt_id=submission_context.attempt.id,
                task_id=submission_context.task_center_task_id,
                outcome="failure",
                summary=summary,
                payload={"failed_criteria": failed_criteria},
            )
        )
    except (TrialSubmissionContextError, TaskCenterInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted evaluation failure.",
        metadata={
            "submission_kind": "evaluator_failure",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
