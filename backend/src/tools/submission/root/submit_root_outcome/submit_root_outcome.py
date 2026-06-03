"""submit_root_outcome terminal tool."""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, Field, field_validator

from sandbox._shared.models import Intent
from task import AgentRole, TaskStatus
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools._hooks.require_no_inflight_background_tasks import (
    RequireNoInflightBackgroundTasks,
)
from .prompt import get_submit_root_outcome_description


RootSubmissionStatus = Literal["success", "failed"]


class SubmitRootOutcomeInput(BaseModel):
    status: RootSubmissionStatus
    outcome: str = Field(..., min_length=1)

    @field_validator("outcome")
    @classmethod
    def _validate_outcome(cls, value: str) -> str:
        if not value or value.isspace():
            raise ValueError("outcome must be nonblank")
        return value


@tool(
    name="submit_root_outcome",
    description=get_submit_root_outcome_description(),
    input_model=SubmitRootOutcomeInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
    pre_hooks=(RequireNoInflightBackgroundTasks("submit_root_outcome"),),
)
async def submit_root_outcome(
    status: RootSubmissionStatus,
    outcome: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    task_store = context.get("task_store")
    if task_store is None:
        return ToolResult(output="Missing request task store.", is_error=True)
    request_id = str(context.get("request_id") or "")
    task_id = str(context.get("task_id") or "")
    if not request_id.strip() or not task_id.strip():
        return ToolResult(output="Missing root request or task id.", is_error=True)

    task: dict[str, Any] | None = task_store.get_task(task_id)
    if task is None:
        return ToolResult(output=f"Root task {task_id!r} was not found.", is_error=True)
    if task.get("request_id") != request_id:
        return ToolResult(output="Root task does not belong to this request.", is_error=True)
    if task.get("workflow_id") is not None:
        return ToolResult(output="submit_root_outcome is only valid for the root task.", is_error=True)
    if task.get("role") != AgentRole.ROOT.value:
        return ToolResult(output=f"Task {task_id!r} is not a root task.", is_error=True)

    task_status = TaskStatus.DONE if status == "success" else TaskStatus.FAILED
    request_status = "done" if status == "success" else "failed"
    task_store.set_task_status(
        task_id,
        status=task_status.value,
        outcomes=[
            {
                "status": status,
                "role": AgentRole.ROOT.value,
                "task_id": task_id,
                "outcome": outcome,
            }
        ],
        terminal_tool_result={
            "status": status,
            "outcome": outcome,
        },
    )
    task_store.finish_request(request_id, status=request_status)
    return ToolResult(
        output=f"Accepted root {status}.",
        metadata={
            "submission_kind": f"root_{'success' if status == 'success' else 'failure'}",
            "request_id": request_id,
            "task_id": task_id,
        },
    )


__all__ = ["SubmitRootOutcomeInput", "submit_root_outcome"]
