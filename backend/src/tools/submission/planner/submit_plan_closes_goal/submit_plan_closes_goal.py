"""submit_plan_closes_goal terminal tool."""

from __future__ import annotations

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
    SUBMISSION_KIND_PLANNER_COMPLETES,
    PlanTaskInput,
    SharedPlannerSubmissionInput,
    build_planner_submission,
)
from .prompt import (
    get_submit_plan_closes_goal_description,
)


class SubmitPlanClosesGoalInput(SharedPlannerSubmissionInput):
    pass


@tool(
    name="submit_plan_closes_goal",
    description=get_submit_plan_closes_goal_description(),
    input_model=SubmitPlanClosesGoalInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_plan_closes_goal"),
        AdvisorApprovalPreHook("submit_plan_closes_goal"),
    ),
)
async def submit_plan_closes_goal(
    plan_spec: str,
    evaluation_criteria: list[str],
    tasks: list[PlanTaskInput],
    task_specs: dict[str, str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_attempt_submission_context(context)
    except AttemptSubmissionContextError as exc:
        return ToolResult(output=str(exc), is_error=True)

    submission, error = build_planner_submission(
        submission_context=submission_context,
        kind="completes",
        plan_spec=plan_spec,
        evaluation_criteria=evaluation_criteria,
        tasks=[PlanTaskInput.model_validate(task) for task in tasks],
        task_specs=task_specs,
        deferred_goal_for_next_iteration=None,
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
            "submission_kind": SUBMISSION_KIND_PLANNER_COMPLETES,
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
