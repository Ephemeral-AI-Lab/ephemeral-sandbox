"""submit_partial_plan terminal tool."""

from __future__ import annotations

from pydantic import Field, field_validator

from task_center import TaskCenterInvariantViolation
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools.submission.context import (
    TrialSubmissionContextError,
    resolve_trial_submission_context,
)
from tools.submission.planner._schemas import (
    PlanTaskInput,
    PlannerSubmissionBaseInput,
    build_planner_submission,
    validate_nonblank,
)


class SubmitPartialPlanInput(PlannerSubmissionBaseInput):
    continuation_goal: str = Field(..., min_length=1)

    @field_validator("continuation_goal")
    @classmethod
    def _validate_continuation_goal(cls, value: str) -> str:
        return validate_nonblank(value, "continuation_goal")


@tool(
    name="submit_partial_plan",
    description="Submit a bounded harness attempt plan with a continuation goal.",
    input_model=SubmitPartialPlanInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
)
async def submit_partial_plan(
    task_specification: str,
    evaluation_criteria: list[str],
    tasks: list[PlanTaskInput],
    task_specs: dict[str, str],
    continuation_goal: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_trial_submission_context(context)
    except TrialSubmissionContextError as exc:
        return ToolResult(output=str(exc), is_error=True)

    submission, error = build_planner_submission(
        submission_context=submission_context,
        kind="partial",
        task_specification=task_specification,
        evaluation_criteria=evaluation_criteria,
        tasks=[PlanTaskInput.model_validate(task) for task in tasks],
        task_specs=task_specs,
        continuation_goal=continuation_goal,
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
            "submission_kind": "planner_partial",
            "task_center_task_id": submission_context.task_center_task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
