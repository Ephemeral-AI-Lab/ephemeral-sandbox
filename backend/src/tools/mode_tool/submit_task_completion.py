"""Terminal tool: executor or evaluator declares the task done."""

from __future__ import annotations

from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.mode_tool._models import SubmissionOutput


class TaskCompletionInput(BaseModel):
    summary: str = Field(
        ...,
        min_length=1,
        description=(
            "Closure summary. For executors with no children: a brief account of "
            "what was done and the verification evidence. For evaluators: the "
            "validation outcome."
        ),
    )


@tool(
    name="submit_task_completion",
    description=(
        "Terminal: declare the current task complete with a summary. The "
        "summary propagates up the closes_for chain to all ancestors that "
        "were gated by this closure."
    ),
    input_model=TaskCompletionInput,
    output_model=SubmissionOutput,
    is_terminal_tool=True,
)
async def submit_task_completion(
    summary: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    tc = context.get("task_center")
    task_id = context.get("task_id")
    if tc is None or task_id is None:
        return ToolResult(
            output="submit_task_completion: missing task_center or task_id in metadata",
            is_error=True,
        )
    tc.submit_task_completion(task_id, summary)
    return ToolResult(output=SubmissionOutput(status="accepted").model_dump_json())
