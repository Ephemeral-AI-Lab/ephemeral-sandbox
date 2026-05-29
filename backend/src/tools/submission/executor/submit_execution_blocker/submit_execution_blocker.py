"""submit_execution_blocker terminal tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from task_center import TaskCenterInvariantViolation
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
    resolve_executor_submission_context,
)
from .prompt import (
    get_submit_execution_blocker_description,
)


class SubmitExecutionBlockerInput(BaseModel):
    summary: str = Field(..., min_length=1)


@tool(
    name="submit_execution_blocker",
    description=get_submit_execution_blocker_description(),
    input_model=SubmitExecutionBlockerInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_execution_blocker"),
        AdvisorApprovalPreHook("submit_execution_blocker"),
    ),
)
async def submit_execution_blocker(
    summary: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_executor_submission_context(context)
        submission_context.submit_executor_blocker(summary=summary)
    except (AttemptSubmissionContextError, TaskCenterInvariantViolation) as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted execution blocker.",
        metadata={
            "submission_kind": "generator_executor_blocker",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt_id,
        },
    )
