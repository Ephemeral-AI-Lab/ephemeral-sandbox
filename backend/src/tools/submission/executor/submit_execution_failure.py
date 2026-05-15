"""submit_execution_failure terminal tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from task_center import TaskCenterInvariantViolation
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools.submission.context import (
    TrialSubmissionContextError,
    resolve_executor_submission_context,
)


class SubmitExecutionFailureInput(BaseModel):
    summary: str = Field(..., min_length=1)
    reason: str = Field(..., min_length=1)
    details: list[str] = Field(default_factory=list)


@tool(
    name="submit_execution_failure",
    description="Submit failed completion of the current generator task.",
    input_model=SubmitExecutionFailureInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
)
async def submit_execution_failure(
    summary: str,
    reason: str,
    details: list[str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_executor_submission_context(context)
        submission_context.submit_executor_failure(
            summary=summary, reason=reason, details=details
        )
    except (TrialSubmissionContextError, TaskCenterInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted execution failure.",
        metadata={
            "submission_kind": "generator_executor_failure",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt_id,
        },
    )
