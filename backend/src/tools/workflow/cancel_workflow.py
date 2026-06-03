"""Cancel delegated workflow work."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from workflow import WorkflowInvariantViolation, WorkflowStatus

from ._runtime import (
    agent_id,
    cancel_workflow_state,
    render_payload,
    require_runtime,
    workflow_manager,
)


class CancelWorkflowInput(BaseModel):
    workflow_task_id: str = Field(..., min_length=1)
    reason: str = Field(default="")


@tool(
    name="cancel_workflow",
    description="Cancel an outstanding delegated workflow by workflow_task_id.",
    input_model=CancelWorkflowInput,
    output_model=TextToolOutput,
    intent=Intent.LIFECYCLE,
    is_terminal_tool=False,
)
async def cancel_workflow(
    workflow_task_id: str,
    reason: str = "",
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        runtime = require_runtime(context)
    except WorkflowInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    manager = workflow_manager(context)
    if manager is None:
        return ToolResult(output="Missing workflow background manager.", is_error=True)
    record = manager.find_workflow_record(
        workflow_id="",
        workflow_task_id=workflow_task_id,
        agent_id=agent_id(context),
    )
    if record is None:
        return ToolResult(
            output=f"Workflow task {workflow_task_id!r} was not found for this agent.",
            is_error=True,
        )
    workflow = runtime.workflow_store.get(record.workflow_id)
    if workflow is None:
        return ToolResult(
            output=f"Workflow {record.workflow_id!r} was not found.",
            is_error=True,
        )
    if workflow.status != WorkflowStatus.CANCELLED:
        workflow = cancel_workflow_state(runtime=runtime, workflow=workflow, reason=reason)
    manager.mark_workflow_cancelled_by_tool(
        workflow_task_id=workflow_task_id,
        reason=reason,
    )
    return ToolResult(
        output=render_payload(
            {
                "workflow_task_id": workflow_task_id,
                "workflow_id": workflow.id,
                "status": workflow.status.value,
                "message": f"Cancelled delegated workflow {workflow_task_id}.",
            }
        )
    )


__all__ = ["CancelWorkflowInput", "cancel_workflow"]
