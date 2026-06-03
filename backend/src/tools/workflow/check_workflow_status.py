"""Render delegated workflow progress or final outcomes."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from workflow import WorkflowInvariantViolation, WorkflowStatus

from ._runtime import (
    agent_id,
    render_payload,
    require_runtime,
    workflow_manager,
    workflow_outcome_records,
    workflow_progress_payload,
)


class CheckWorkflowStatusInput(BaseModel):
    workflow_id: str = Field(..., min_length=1)
    workflow_task_id: str | None = Field(default=None)


@tool(
    name="check_workflow_status",
    description="Inspect delegated workflow progress and print terminal outcomes when available.",
    input_model=CheckWorkflowStatusInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=False,
)
async def check_workflow_status(
    workflow_id: str,
    workflow_task_id: str | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    try:
        runtime = require_runtime(context)
    except WorkflowInvariantViolation as exc:
        return ToolResult(output=str(exc), is_error=True)

    workflow = runtime.workflow_store.get(workflow_id)
    if workflow is None:
        return ToolResult(output=f"Workflow {workflow_id!r} was not found.", is_error=True)

    manager = workflow_manager(context)
    record = None
    if manager is not None:
        outcomes = workflow_outcome_records(workflow)
        manager.refresh_workflow_status(
            workflow_id=workflow.id,
            status=workflow.status.value,
            outcomes=outcomes,
        )
        record = manager.find_workflow_record(
            workflow_id=workflow.id,
            workflow_task_id=workflow_task_id,
            agent_id=agent_id(context),
        )
        if workflow_task_id and record is None:
            return ToolResult(
                output=(
                    f"Workflow task {workflow_task_id!r} does not match workflow "
                    f"{workflow.id!r} for this agent."
                ),
                is_error=True,
            )
        if record is not None:
            workflow_task_id = record.workflow_task_id

    payload = workflow_progress_payload(
        runtime=runtime,
        workflow=workflow,
        workflow_task_id=workflow_task_id,
        record_status=record.status.value if record is not None else None,
    )

    if manager is not None and record is not None and workflow.status != WorkflowStatus.OPEN:
        manager.mark_workflow_reported_by_status_tool(record.workflow_task_id)
        payload["status"] = workflow.status.value
        payload["workflow_task_id"] = record.workflow_task_id

    return ToolResult(output=render_payload(payload))


__all__ = ["CheckWorkflowStatusInput", "check_workflow_status"]
