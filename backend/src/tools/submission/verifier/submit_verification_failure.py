"""submit_verification_failure terminal tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from task_center import (
    GeneratorSubmission,
    TaskCenterInvariantViolation,
)
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools.submission.context import (
    TrialSubmissionContextError,
    resolve_trial_submission_context,
)


class SubmitVerificationFailureInput(BaseModel):
    summary: str = Field(..., min_length=1)
    unresolved_issues: list[str] = Field(default_factory=list)


@tool(
    name="submit_verification_failure",
    description="Submit failed verification of the current generator task.",
    input_model=SubmitVerificationFailureInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
)
async def submit_verification_failure(
    summary: str,
    unresolved_issues: list[str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_trial_submission_context(context)
        submission_context.orchestrator.apply_generator_submission(
            GeneratorSubmission(
                attempt_id=submission_context.attempt.id,
                task_id=submission_context.task_center_task_id,
                outcome="failure",
                summary=summary,
                payload={
                    "generator_role": "verifier",
                    "unresolved_issues": unresolved_issues,
                },
            )
        )
    except (TrialSubmissionContextError, TaskCenterInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted verification failure.",
        metadata={
            "submission_kind": "generator_verifier_failure",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
