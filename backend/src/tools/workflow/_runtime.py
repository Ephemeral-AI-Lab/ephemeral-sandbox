"""Shared helpers for delegated workflow tools."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from typing import Any

from engine.background.task_supervisor import BackgroundTaskSupervisor
from sandbox._shared.models import Intent
from task import TaskStatus
from tools._framework.core.context import ToolExecutionContextService
from tools._hooks._context import resolve_agent_id
from workflow import AttemptDeps, WorkflowInvariantViolation
from workflow._core.outcomes import parse_outcomes_record, to_record
from workflow._core.state import (
    AttemptFailReason,
    AttemptStatus,
    IterationStatus,
    Workflow,
    WorkflowStatus,
)


def require_runtime(context: ToolExecutionContextService) -> AttemptDeps:
    runtime = context.get("attempt_runtime")
    if not isinstance(runtime, AttemptDeps):
        raise WorkflowInvariantViolation("Missing workflow runtime.")
    return runtime


def require_parent_task(
    context: ToolExecutionContextService,
    runtime: AttemptDeps,
) -> dict[str, Any]:
    task_id = str(context.get("task_id") or "")
    if not task_id.strip():
        raise WorkflowInvariantViolation("Missing parent task id.")
    task = runtime.task_store.get_task(task_id)
    if task is None:
        raise WorkflowInvariantViolation(f"Parent task {task_id!r} was not found.")
    if task.get("status") != TaskStatus.RUNNING.value:
        raise WorkflowInvariantViolation(
            f"Parent task {task_id!r} is not running; workflow delegation requires a running task."
        )
    return task


def workflow_manager(
    context: ToolExecutionContextService,
) -> BackgroundTaskSupervisor | None:
    manager = context.get("background_task_manager")
    if isinstance(manager, BackgroundTaskSupervisor):
        return manager
    return None


def agent_id(context: ToolExecutionContextService) -> str:
    return resolve_agent_id(context)


def workflow_outcome_records(workflow: Workflow) -> list[dict[str, Any]]:
    return [to_record(outcome) for outcome in parse_outcomes_record(workflow.outcomes)]


def workflow_progress_payload(
    *,
    runtime: AttemptDeps,
    workflow: Workflow,
    workflow_task_id: str | None = None,
    record_status: str | None = None,
) -> dict[str, Any]:
    iterations = runtime.iteration_store.list_for_workflow(workflow.id)
    current_iteration = iterations[-1] if iterations else None
    attempts = (
        runtime.attempt_store.list_for_iteration(current_iteration.id)
        if current_iteration is not None
        else []
    )
    current_attempt = attempts[-1] if attempts else None
    tasks = (
        runtime.task_store.list_tasks_for_attempt(current_attempt.id)
        if current_attempt is not None
        else []
    )
    outcomes = workflow_outcome_records(workflow)
    status = workflow.status.value
    payload = {
        "workflow_task_id": workflow_task_id,
        "workflow_id": workflow.id,
        "status": status,
        "progress": _progress_text(workflow, len(tasks), outcomes, record_status),
        "workflow": {
            "status": status,
            "current_iteration_id": current_iteration.id if current_iteration else None,
            "current_attempt_id": current_attempt.id if current_attempt else None,
            "tasks": [
                {
                    "task_id": task["task_id"],
                    "role": task.get("role"),
                    "status": task.get("status"),
                    "agent_name": task.get("agent_name"),
                }
                for task in tasks
            ],
        },
        "outcomes": outcomes,
    }
    if workflow_task_id is None:
        payload.pop("workflow_task_id")
    return payload


def render_payload(payload: dict[str, Any]) -> str:
    return json.dumps(payload, indent=2, sort_keys=True)


def cancel_workflow_state(
    *,
    runtime: AttemptDeps,
    workflow: Workflow,
    reason: str,
) -> Workflow:
    now = datetime.now(UTC)
    cancelled_outcomes = json.dumps(
        [
            {
                "status": "failed",
                "role": "workflow",
                "task_id": workflow.id,
                "outcome": reason or "Delegated workflow was cancelled.",
            }
        ]
    )
    for iteration in runtime.iteration_store.list_for_workflow(workflow.id):
        if iteration.is_open:
            for attempt in runtime.attempt_store.list_for_iteration(iteration.id):
                if not attempt.is_closed:
                    for task in runtime.task_store.list_tasks_for_attempt(attempt.id):
                        if task.get("status") in {
                            TaskStatus.PENDING.value,
                            TaskStatus.RUNNING.value,
                        }:
                            runtime.task_store.set_task_status_if_current(
                                task["task_id"],
                                expected_status=task["status"],
                                status=TaskStatus.FAILED.value,
                                outcomes=[
                                    {
                                        "status": "failed",
                                        "role": task.get("role"),
                                        "task_id": task["task_id"],
                                        "outcome": reason
                                        or "Delegated workflow was cancelled.",
                                    }
                                ],
                                terminal_tool_result={"fail_reason": "workflow_cancelled"},
                            )
                    runtime.attempt_store.close(
                        attempt.id,
                        status=AttemptStatus.FAILED,
                        fail_reason=AttemptFailReason.TASK_FAILED,
                        outcomes=[],
                        closed_at=now,
                    )
            runtime.iteration_store.set_status(
                iteration.id,
                status=IterationStatus.CANCELLED,
                closed_at=now,
                outcomes=cancelled_outcomes,
            )
    return runtime.workflow_store.set_status(
        workflow.id,
        status=WorkflowStatus.CANCELLED,
        closed_at=now,
        outcomes=cancelled_outcomes,
    )


def _progress_text(
    workflow: Workflow,
    task_count: int,
    outcomes: list[dict[str, Any]],
    record_status: str | None,
) -> str:
    if workflow.status == WorkflowStatus.OPEN:
        return f"Workflow is running with {task_count} current task(s)."
    delivered = f" Background status: {record_status}." if record_status else ""
    return (
        f"Workflow {workflow.status.value} with {len(outcomes)} outcome record(s)."
        f"{delivered}"
    )


__all__ = [
    "Intent",
    "agent_id",
    "cancel_workflow_state",
    "render_payload",
    "require_parent_task",
    "require_runtime",
    "workflow_manager",
    "workflow_outcome_records",
    "workflow_progress_payload",
]
