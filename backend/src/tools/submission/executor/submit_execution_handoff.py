"""submit_execution_handoff delegated request tool."""

from __future__ import annotations

from typing import TYPE_CHECKING

from pydantic import BaseModel, Field, field_validator

from task_center import TaskCenterInvariantViolation
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools.submission.context import (
    TrialSubmissionContextError,
    resolve_executor_submission_context,
)

if TYPE_CHECKING:
    from task_center import StartedMission


class RequestMissionSolutionInput(BaseModel):
    goal: str = Field(..., min_length=1)

    @field_validator("goal")
    @classmethod
    def _validate_goal(cls, value: str) -> str:
        if not value or value.isspace():
            raise ValueError("goal must be nonblank")
        return value


@tool(
    name="submit_execution_handoff",
    description=(
        "Request a delegated complex-task solution for the current generator task. "
        "This must be called before making edits."
    ),
    input_model=RequestMissionSolutionInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
)
async def submit_execution_handoff(
    goal: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        submission_context = resolve_executor_submission_context(context)
    except TrialSubmissionContextError as exc:
        return ToolResult(output=str(exc), is_error=True)

    try:
        started_mission: StartedMission = (
            submission_context.start_delegated_mission(goal=goal)
        )
    except TaskCenterInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    return ToolResult(
        output=(
            "Started delegated mission "
            f"{started_mission.mission_id} "
            "for this generator task."
        ),
        metadata={
            "submission_kind": "mission_start",
            "task_center_task_id": started_mission.parent_task_id,
            "attempt_id": started_mission.parent_attempt_id,
            "goal_id": started_mission.mission_id,
            "initial_episode_id": started_mission.initial_episode_id,
            "initial_attempt_id": started_mission.initial_attempt_id,
        },
    )
