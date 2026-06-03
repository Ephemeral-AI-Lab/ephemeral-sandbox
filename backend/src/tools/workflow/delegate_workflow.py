"""Start a delegated workflow without ending the caller's agent run."""

from __future__ import annotations

from pydantic import BaseModel, Field, field_validator

from sandbox._shared.models import Intent
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from workflow import WorkflowInvariantViolation, WorkflowStarter

from ._runtime import (
    agent_id,
    render_payload,
    require_parent_task,
    require_runtime,
    workflow_manager,
)


class DelegateWorkflowInput(BaseModel):
    goal: str = Field(
        ...,
        min_length=1,
        description="Delegated workflow goal, including relevant findings and constraints.",
    )

    @field_validator("goal")
    @classmethod
    def _validate_goal(cls, value: str) -> str:
        if not value or value.isspace():
            raise ValueError("goal must be nonblank")
        return value


@tool(
    name="delegate_workflow",
    description=(
        "Start non-terminal delegated workflow work. Returns a workflow handle; "
        "continue running and use check_workflow_status or cancel_workflow later."
    ),
    input_model=DelegateWorkflowInput,
    output_model=TextToolOutput,
    intent=Intent.LIFECYCLE,
    is_terminal_tool=False,
)
async def delegate_workflow(
    goal: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        runtime = require_runtime(context)
        parent_task = require_parent_task(context, runtime)
    except WorkflowInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    manager = workflow_manager(context)
    caller_agent_id = agent_id(context)
    parent_task_id = str(parent_task["task_id"])
    if manager is not None:
        existing = manager.find_outstanding_workflow_for_parent(
            parent_task_id=parent_task_id,
            agent_id=caller_agent_id,
        )
        if existing is not None:
            return ToolResult(
                output=render_payload(
                    {
                        "workflow_task_id": existing.workflow_task_id,
                        "workflow_id": existing.workflow_id,
                        "status": existing.status.value,
                        "message": (
                            "A delegated workflow is already outstanding for this task. "
                            "Use check_workflow_status or cancel_workflow before starting another."
                        ),
                    }
                ),
                is_error=True,
            )

    try:
        started = WorkflowStarter(runtime=runtime).start(
            prompt=goal,
            parent_task_id=parent_task_id,
        )
    except WorkflowInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    workflow_task_id = started.workflow_id
    if manager is not None:
        record = manager.register_workflow(
            workflow_id=started.workflow_id,
            parent_task_id=started.parent_task_id,
            parent_attempt_id=started.parent_attempt_id,
            request_id=str(parent_task["request_id"]),
            agent_id=caller_agent_id,
            goal=goal,
        )
        workflow_task_id = record.workflow_task_id

    return ToolResult(
        output=render_payload(
            {
                "workflow_task_id": workflow_task_id,
                "workflow_id": started.workflow_id,
                "status": "running",
                "message": (
                    f"Started delegated workflow {workflow_task_id}. "
                    "Use check_workflow_status to inspect progress or cancel_workflow to stop it."
                ),
            }
        ),
        metadata={
            "submission_kind": "workflow_delegated",
            "workflow_task_id": workflow_task_id,
            "workflow_id": started.workflow_id,
            "task_id": started.parent_task_id,
            "attempt_id": started.parent_attempt_id,
            "initial_iteration_id": started.iteration_id,
            "initial_attempt_id": started.attempt_id,
        },
    )


__all__ = ["DelegateWorkflowInput", "delegate_workflow"]
