"""Terminal tool: executor hands off a full DAG plan."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from task_center.errors import PlanValidationError
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from tools.submission._models import SubmissionOutput, TaskDependencyEntry, TaskSpec


class FullPlanHandoffInput(BaseModel):
    tasks: list[TaskDependencyEntry] = Field(
        ...,
        description=(
            "Flat DAG plan: each entry is {id, deps}. List only DIRECT deps; "
            "transitive predecessors are implicit."
        ),
    )
    task_specs: dict[str, TaskSpec] = Field(
        ...,
        description="Map of task id -> {title, spec}. Every entry id must be a key here.",
    )
    acceptance_criteria: str = Field(
        ...,
        min_length=1,
        description=(
            "Immutable success criteria for the handoff. The evaluator validates "
            "child outputs against this text."
        ),
    )


@tool(
    name="submit_full_plan_handoff",
    description=(
        "Terminal: hand off the full task as a DAG plan. Use when the plan "
        "fully covers the acceptance_criteria. TaskCenter compiles the DAG, "
        "spawns child executors as deps complete, and runs one final evaluator "
        "after every sink task is DONE."
    ),
    input_model=FullPlanHandoffInput,
    output_model=SubmissionOutput,
)
async def submit_full_plan_handoff(
    tasks: list[dict[str, Any]],
    task_specs: dict[str, dict[str, Any]],
    acceptance_criteria: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    tc = context.metadata.get("task_center")
    task_id = context.metadata.get("task_id")
    if tc is None or task_id is None:
        return ToolResult(
            output="submit_full_plan_handoff: missing task_center or task_id in metadata",
            is_error=True,
        )
    try:
        tc.submit_full_handoff(task_id, tasks, task_specs, acceptance_criteria)
    except PlanValidationError as exc:
        return ToolResult(output=f"plan rejected: {exc}", is_error=True)
    return ToolResult(output="accepted")
