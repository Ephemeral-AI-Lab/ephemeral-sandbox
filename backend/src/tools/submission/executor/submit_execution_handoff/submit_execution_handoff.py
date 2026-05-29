"""submit_execution_handoff terminal tool.

Hands the executor task back to the planner for goal decomposition when
the current goal's scope is too large for a single executor pass. The
``goal_handoff`` arg is the statement of the goal that needs to be
decomposed (verbatim or paraphrased without information loss), together
with the executor's findings and reasons for the handoff. It flows
through to ``WorkflowStarter.start(prompt=...)`` and becomes the statement
of a new delegated Workflow — not a summary of the current attempt.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from pydantic import BaseModel, Field, field_validator

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
    get_submit_execution_handoff_description,
)

if TYPE_CHECKING:
    from task_center import StartedWorkflow


class SubmitExecutionHandoffInput(BaseModel):
    goal_handoff: str = Field(
        ...,
        min_length=1,
        description=(
            "The original goal statement (verbatim or paraphrased "
            "without information loss), plus your findings and the "
            "reasons it needs to be decomposed by the planner."
        ),
    )

    @field_validator("goal_handoff")
    @classmethod
    def _validate_goal_handoff(cls, value: str) -> str:
        if not value or value.isspace():
            raise ValueError("goal_handoff must be nonblank")
        return value


@tool(
    name="submit_execution_handoff",
    description=get_submit_execution_handoff_description(),
    input_model=SubmitExecutionHandoffInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_execution_handoff"),
        AdvisorApprovalPreHook("submit_execution_handoff"),
    ),
)
async def submit_execution_handoff(
    goal_handoff: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_executor_submission_context(context)
    except AttemptSubmissionContextError as exc:
        return ToolResult(output=str(exc), is_error=True)

    try:
        started_workflow: StartedWorkflow = (
            submission_context.start_delegated_workflow(goal_handoff=goal_handoff)
        )
    except TaskCenterInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output=(
            "Started delegated workflow "
            f"{started_workflow.workflow_id} "
            "for this generator task."
        ),
        metadata={
            "submission_kind": "workflow_start",
            "task_center_task_id": started_workflow.origin.task_id,
            "attempt_id": started_workflow.parent_attempt_id,
            "workflow_id": started_workflow.workflow_id,
            "initial_iteration_id": started_workflow.initial_iteration_id,
            "initial_attempt_id": started_workflow.initial_attempt_id,
        },
    )
