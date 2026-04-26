"""Terminal handoff tool (evaluator-only): expand into continuation work."""

from __future__ import annotations

from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.mode_tool._models import SubmissionOutput


class ContinueWorkHandoffInput(BaseModel):
    task_input: str = Field(
        ...,
        min_length=1,
        description=(
            "Input for the continuation executor: which acceptance_criteria are "
            "not yet satisfied, what gap remains, and what to focus on."
        ),
    )


@tool(
    name="submit_continue_work_handoff",
    description=(
        "Terminal handoff (evaluator-only): expand the evaluator into continuation work. "
        "TaskCenter spawns a continuation executor under the evaluator; the "
        "original executor remains handed off until the continuation chain closes."
    ),
    input_model=ContinueWorkHandoffInput,
    output_model=SubmissionOutput,
    is_terminal_tool=True,
)
async def submit_continue_work_handoff(
    task_input: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    role = context.get("role")
    if role != "evaluator":
        return ToolResult(
            output=(
                "submit_continue_work_handoff is evaluator-only "
                f"(current role={role!r}); executors must use submit_task_completion "
                "or one of the handoff tools instead."
            ),
            is_error=True,
        )
    tc = context.get("task_center")
    task_id = context.get("task_id")
    if tc is None or task_id is None:
        return ToolResult(
            output="submit_continue_work_handoff: missing task_center or task_id in metadata",
            is_error=True,
        )
    tc.submit_continue_work_handoff(task_id, task_input)
    return ToolResult(output=SubmissionOutput(status="accepted").model_dump_json())
