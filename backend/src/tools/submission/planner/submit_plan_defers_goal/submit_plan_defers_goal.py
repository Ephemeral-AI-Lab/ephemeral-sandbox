"""submit_plan_defers_goal terminal tool."""

from __future__ import annotations

from pydantic import Field, field_validator

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
    resolve_attempt_submission_context,
)
from tools.submission.planner._schemas import (
    SUBMISSION_KIND_PLANNER_DEFERS,
    PlanTaskInput,
    SharedPlannerSubmissionInput,
    build_planner_submission,
    validate_nonblank,
)
from .prompt import (
    get_submit_plan_defers_goal_description,
)


class SubmitPlanDefersGoalInput(SharedPlannerSubmissionInput):
    deferred_goal_for_next_iteration: str = Field(..., min_length=1)

    @field_validator("deferred_goal_for_next_iteration")
    @classmethod
    def _validate_deferred_goal_for_next_iteration(cls, value: str) -> str:
        return validate_nonblank(value, "deferred_goal_for_next_iteration")


@tool(
    name="submit_plan_defers_goal",
    description=get_submit_plan_defers_goal_description(),
    input_model=SubmitPlanDefersGoalInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_plan_defers_goal"),
        AdvisorApprovalPreHook("submit_plan_defers_goal"),
    ),
)
async def submit_plan_defers_goal(
    plan_spec: str,
    evaluation_criteria: list[str],
    tasks: list[PlanTaskInput],
    task_specs: dict[str, str],
    deferred_goal_for_next_iteration: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_attempt_submission_context(context)
    except AttemptSubmissionContextError as exc:
        return ToolResult(output=str(exc), is_error=True)

    submission, error = build_planner_submission(
        submission_context=submission_context,
        kind="defers",
        plan_spec=plan_spec,
        evaluation_criteria=evaluation_criteria,
        tasks=[PlanTaskInput.model_validate(task) for task in tasks],
        task_specs=task_specs,
        deferred_goal_for_next_iteration=deferred_goal_for_next_iteration,
    )
    if error is not None or submission is None:
        return ToolResult(output=error or "Invalid planner submission.", is_error=True)

    try:
        submission_context.orchestrator.apply_plan_submission(submission)
    except TaskCenterInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted planner submission.",
        metadata={
            "submission_kind": SUBMISSION_KIND_PLANNER_DEFERS,
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
