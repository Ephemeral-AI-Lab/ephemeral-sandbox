"""submit_planner_outcome terminal tool."""

from __future__ import annotations

from pydantic import Field, field_validator

from sandbox._shared.models import Intent
from workflow import WorkflowInvariantViolation
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools._hooks.advisor_approval import AdvisorApprovalPreHook
from tools._hooks.disallow_nested_planner_deferral import (
    DisallowNestedPlannerDeferral,
)
from tools._hooks.require_no_inflight_background_tasks import (
    RequireNoInflightBackgroundTasks,
)
from tools.submission.context import (
    AttemptSubmissionContextError,
    resolve_attempt_submission_context,
)
from tools.submission.planner._schemas import (
    SUBMISSION_KIND_PLANNER_COMPLETES,
    SUBMISSION_KIND_PLANNER_DEFERS,
    PlanTaskInput,
    ReducerInput,
    SharedPlannerSubmissionInput,
    build_planner_submission,
    planner_kind_from_deferred_goal,
    validate_nonblank,
)
from .prompt import get_submit_planner_outcome_description


class SubmitPlannerOutcomeInput(SharedPlannerSubmissionInput):
    deferred_goal_for_next_iteration: str | None = Field(
        default=None,
        description=(
            "Concrete goal items from the current iteration goal that this plan "
            "intentionally leaves for the next iteration. Omit or null means this "
            "plan covers all current-iteration goal items and leaves no remaining items."
        ),
    )

    @field_validator("deferred_goal_for_next_iteration")
    @classmethod
    def _validate_deferred_goal_for_next_iteration(
        cls, value: str | None
    ) -> str | None:
        if value is None:
            return None
        return validate_nonblank(value, "deferred_goal_for_next_iteration")


@tool(
    name="submit_planner_outcome",
    description=get_submit_planner_outcome_description(),
    input_model=SubmitPlannerOutcomeInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(
        RequireNoInflightBackgroundTasks("submit_planner_outcome"),
        DisallowNestedPlannerDeferral("submit_planner_outcome"),
        AdvisorApprovalPreHook("submit_planner_outcome"),
    ),
)
async def submit_planner_outcome(
    tasks: list[PlanTaskInput],
    task_specs: dict[str, str],
    reducers: list[ReducerInput],
    deferred_goal_for_next_iteration: str | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_attempt_submission_context(context)
    except AttemptSubmissionContextError as exc:
        return ToolResult(output=str(exc), is_error=True)

    kind, normalized_deferred_goal = planner_kind_from_deferred_goal(
        deferred_goal_for_next_iteration
    )
    submission, error = build_planner_submission(
        submission_context=submission_context,
        kind=kind,
        tasks=[PlanTaskInput.model_validate(task) for task in tasks],
        task_specs=task_specs,
        reducers=[ReducerInput.model_validate(reducer) for reducer in reducers],
        deferred_goal_for_next_iteration=normalized_deferred_goal,
    )
    if error is not None or submission is None:
        return ToolResult(output=error or "Invalid planner submission.", is_error=True)

    try:
        submission_context.orchestrator.apply_plan_submission(submission)
    except WorkflowInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output="Accepted planner submission.",
        metadata={
            "submission_kind": (
                SUBMISSION_KIND_PLANNER_DEFERS
                if kind == "defers"
                else SUBMISSION_KIND_PLANNER_COMPLETES
            ),
            "task_id": submission_context.task_id,
            "attempt_id": submission_context.attempt.id,
        },
    )
