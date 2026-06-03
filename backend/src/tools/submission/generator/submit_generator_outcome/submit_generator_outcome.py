"""submit_generator_outcome terminal tool."""

from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field, field_validator

from sandbox._shared.models import Intent
from workflow import WorkflowInvariantViolation
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools._hooks.advisor_approval import AdvisorApprovalPreHook
from tools._hooks.require_no_inflight_background_tasks import (
    RequireNoInflightBackgroundTasks,
)
from tools.submission.context import (
    AttemptSubmissionContextError,
    resolve_generator_submission_context,
)
from .prompt import get_submit_generator_outcome_description


GeneratorSubmissionStatus = Literal["success", "failed"]


class SubmitGeneratorOutcomeInput(BaseModel):
    status: GeneratorSubmissionStatus
    outcome: str = Field(..., min_length=1)

    @field_validator("outcome")
    @classmethod
    def _validate_outcome(cls, value: str) -> str:
        if not value or value.isspace():
            raise ValueError("outcome must be nonblank")
        return value


@tool(
    name="submit_generator_outcome",
    description=get_submit_generator_outcome_description(),
    input_model=SubmitGeneratorOutcomeInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_generator_outcome"),
        AdvisorApprovalPreHook("submit_generator_outcome"),
    ),
)
async def submit_generator_outcome(
    status: GeneratorSubmissionStatus,
    outcome: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_generator_submission_context(context)
        submission_context.submit_generator_outcome(status=status, outcome=outcome)
    except (AttemptSubmissionContextError, WorkflowInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output=f"Accepted generator {status}.",
        metadata={
            "submission_kind": f"generator_{'success' if status == 'success' else 'failure'}",
            "task_id": submission_context.task_id,
            "attempt_id": submission_context.attempt_id,
        },
    )


__all__ = ["SubmitGeneratorOutcomeInput", "submit_generator_outcome"]
