"""Terminal tool: executor hands off a DAG plan with required handoff_note."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from task_center.errors import PlanValidationError
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.mode_tool._models import SubmissionOutput, TaskDependencyEntry, TaskSpec


class PlanHandoffInput(BaseModel):
    tasks: list[TaskDependencyEntry] = Field(
        ...,
        description=(
            "Flat DAG plan: each entry is {id, deps}. List only DIRECT deps; "
            "transitive predecessors are implicit."
        ),
    )
    task_specs: dict[str, TaskSpec] = Field(
        ...,
        description=(
            "Map of task id -> {title, task_input}. Every entry id must be a key here."
        ),
    )
    acceptance_criteria: str = Field(
        ...,
        min_length=1,
        description=(
            "Immutable success criteria for the handoff. The evaluator validates "
            "child outputs against this text."
        ),
    )
    handoff_note: str = Field(
        ...,
        min_length=1,
        description=(
            "Articulation of what the plan covers, what remains uncertain, "
            "which acceptance_criteria items are most fragile, and what "
            "evidence the evaluator should inspect. Required on every "
            "handoff — the evaluator validates against acceptance_criteria "
            "regardless, so this note is for context, not gating."
        ),
    )


@tool(
    name="submit_plan_handoff",
    description=(
        "Terminal: hand off the task as a DAG plan with a required "
        "handoff_note. TaskCenter compiles the DAG, spawns child executors as "
        "deps complete, runs an evaluator after every sink task is DONE, and "
        "the evaluator reads handoff_note before validating against "
        "acceptance_criteria."
    ),
    input_model=PlanHandoffInput,
    output_model=SubmissionOutput,
    is_terminal_tool=True,
)
async def submit_plan_handoff(
    tasks: list[dict[str, Any]],
    task_specs: dict[str, dict[str, Any]],
    acceptance_criteria: str,
    handoff_note: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    tc = context.get("task_center")
    task_id = context.get("task_id")
    if tc is None or task_id is None:
        return ToolResult(
            output="submit_plan_handoff: missing task_center or task_id in metadata",
            is_error=True,
        )
    try:
        tc.submit_plan_handoff(
            task_id, tasks, task_specs, acceptance_criteria, handoff_note
        )
    except PlanValidationError as exc:
        return ToolResult(output=f"plan rejected: {exc}", is_error=True)
    return ToolResult(output=SubmissionOutput(status="accepted").model_dump_json())
