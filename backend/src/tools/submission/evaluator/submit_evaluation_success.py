"""submit_evaluation_success terminal tool."""

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


class SubmitEvaluationSuccessInput(BaseModel):
    summary: str = Field(..., min_length=1)
    passed_criteria: list[str] = Field(default_factory=list)


@tool(
    name="submit_evaluation_success",
    description="Submit attempt-level evaluation success.",
    input_model=SubmitEvaluationSuccessInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
)
async def submit_evaluation_success(
    summary: str,
    passed_criteria: list[str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_trial_submission_context(context)
        submission_context.orchestrator.apply_evaluator_submission(
            EvaluatorSubmission(
                attempt_id=submission_context.attempt.id,
                task_id=submission_context.task_center_task_id,
                outcome="success",
                summary=summary,
                payload={"passed_criteria": passed_criteria},
            )
        )
    except (TrialSubmissionContextError, TaskCenterInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted evaluation success.",
        metadata={
            "submission_kind": "evaluator_success",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
