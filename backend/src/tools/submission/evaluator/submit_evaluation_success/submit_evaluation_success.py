"""submit_evaluation_success terminal tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from task_center import (
    EvaluatorSubmission,
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
    get_submit_evaluation_success_description,
)


class SubmitEvaluationSuccessInput(BaseModel):
    summary: str = Field(..., min_length=1)
    passed_criteria: list[str] = Field(default_factory=list)


@tool(
    name="submit_evaluation_success",
    description=get_submit_evaluation_success_description(),
    input_model=SubmitEvaluationSuccessInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_evaluation_success"),
        AdvisorApprovalPreHook("submit_evaluation_success"),
    ),
)
async def submit_evaluation_success(
    summary: str,
    passed_criteria: list[str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_attempt_submission_context(context)
        submission_context.orchestrator.apply_evaluator_submission(
            EvaluatorSubmission(
                attempt_id=submission_context.attempt.id,
                task_id=submission_context.task_center_task_id,
                outcome="success",
                summary=summary,
                payload={"passed_criteria": passed_criteria},
            )
        )
    except (AttemptSubmissionContextError, TaskCenterInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted evaluation success.",
        metadata={
            "submission_kind": "evaluator_success",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
