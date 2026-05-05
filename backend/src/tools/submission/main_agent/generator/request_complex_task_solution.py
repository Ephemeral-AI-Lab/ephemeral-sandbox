"""request_complex_task_solution delegated request tool."""

from __future__ import annotations

from pydantic import BaseModel, Field, field_validator

from task_center.mission.starter import StartedMissionRequest
from task_center.exceptions import GraphInvariantViolation
from task_center.task import HarnessTaskRole
from tools.core.context import ToolExecutionContextService
from tools.core.decorator import tool
from tools.core.results import TextToolOutput, ToolResult
from tools.submission.context import (
    HarnessSubmissionContextError,
    resolve_executor_submission_context,
)
from tools.submission.hooks import (
    HarnessAgentProfileGate,
    HarnessRoleGate,
    RequestComplexTaskBeforeEditGate,
)


class RequestComplexTaskSolutionInput(BaseModel):
    goal: str = Field(..., min_length=1)

    @field_validator("goal")
    @classmethod
    def _validate_goal(cls, value: str) -> str:
        if not value or value.isspace():
            raise ValueError("goal must be nonblank")
        return value


@tool(
    name="request_complex_task_solution",
    description=(
        "Request a delegated complex-task solution for the current generator task. "
        "This must be called before making edits."
    ),
    input_model=RequestComplexTaskSolutionInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
    pre_hooks=(
        HarnessRoleGate("request_complex_task_solution", HarnessTaskRole.GENERATOR),
        HarnessAgentProfileGate(
            target_tool="request_complex_task_solution",
            expected_profile_role="executor",
        ),
        RequestComplexTaskBeforeEditGate(),
    ),
)
async def request_complex_task_solution(
    goal: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_executor_submission_context(context)
    except HarnessSubmissionContextError as exc:
        return ToolResult(output=str(exc), is_error=True)

    try:
        started_request: StartedMissionRequest = (
            submission_context.start_mission_request(goal=goal)
        )
    except GraphInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output=(
            "Started delegated mission request "
            f"{started_request.complex_task_request_id} "
            "for this generator task."
        ),
        metadata={
            "submission_kind": "complex_task_request_start",
            "task_center_task_id": started_request.parent_task_id,
            "harness_graph_id": started_request.parent_harness_graph_id,
            "complex_task_request_id": started_request.complex_task_request_id,
            "initial_segment_id": started_request.initial_segment_id,
            "initial_harness_graph_id": started_request.initial_harness_graph_id,
        },
    )
