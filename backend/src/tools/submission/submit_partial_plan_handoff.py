"""Terminal tool: executor hands off a partial DAG plan with handoff_note."""

from __future__ import annotations

from typing import Any

from pydantic import Field

from task_center.errors import PlanValidationError
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from tools.submission._models import SubmissionOutput
from tools.submission.submit_full_plan_handoff import FullPlanHandoffInput


class PartialPlanHandoffInput(FullPlanHandoffInput):
    handoff_note: str = Field(
        ...,
        min_length=1,
        description=(
            "Required explanation of: what the plan covers, what remains "
            "uncertain, which parts of acceptance_criteria may stay unsatisfied, "
            "and what evidence the evaluator should inspect."
        ),
    )


@tool(
    name="submit_partial_plan_handoff",
    description=(
        "Terminal: hand off useful DAG work that does NOT fully cover the "
        "acceptance_criteria. handoff_note is required; the evaluator reads it "
        "before deciding whether to complete or continue."
    ),
    input_model=PartialPlanHandoffInput,
    output_model=SubmissionOutput,
)
async def submit_partial_plan_handoff(
    tasks: list[dict[str, Any]],
    task_specs: dict[str, dict[str, Any]],
    acceptance_criteria: str,
    handoff_note: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    tc = context.metadata.get("task_center")
    task_id = context.metadata.get("task_id")
    if tc is None or task_id is None:
        return ToolResult(
            output="submit_partial_plan_handoff: missing task_center or task_id in metadata",
            is_error=True,
        )
    try:
        tc.submit_partial_handoff(
            task_id, tasks, task_specs, acceptance_criteria, handoff_note
        )
    except PlanValidationError as exc:
        return ToolResult(output=f"plan rejected: {exc}", is_error=True)
    return ToolResult(output="accepted")
