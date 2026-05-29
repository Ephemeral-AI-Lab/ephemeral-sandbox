"""submit_verification_failure terminal tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from task_center import (
    GeneratorSubmission,
    TaskCenterInvariantViolation,
)
from tools._framework.core.context import ToolExecutionContextService
from sandbox.shared.models import Intent
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools._hooks.require_no_inflight_background_tasks import (
    RequireNoInflightBackgroundTasks,
)
from tools._hooks.advisor_approval import AdvisorApprovalPreHook
from tools.submission.context import (
    AttemptSubmissionContextError,
    resolve_attempt_submission_context,
)
from .prompt import (
    get_submit_verification_failure_description,
)


class SubmitVerificationFailureInput(BaseModel):
    summary: str = Field(..., min_length=1)
    unresolved_issues: list[str] = Field(default_factory=list)


@tool(
    name="submit_verification_failure",
    description=get_submit_verification_failure_description(),
    input_model=SubmitVerificationFailureInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_verification_failure"),
        AdvisorApprovalPreHook("submit_verification_failure"),
    ),
)
async def submit_verification_failure(
    summary: str,
    unresolved_issues: list[str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_attempt_submission_context(context)
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
    except (AttemptSubmissionContextError, TaskCenterInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted verification failure.",
        metadata={
            "submission_kind": "generator_verifier_failure",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
