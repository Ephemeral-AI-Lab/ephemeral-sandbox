"""Workflow ancestry — nested workflow depth resolution.

Walks the parent-task / parent-attempt / parent-iteration chain to count how
many workflows deep a given workflow sits. Used by the agent-routing predicates
that still need nested workflow awareness.
"""

from __future__ import annotations

from task_center._core.persistence import (
    AttemptStoreProtocol,
    WorkflowStoreProtocol,
    IterationStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.primitives import TaskCenterInvariantViolation


def nested_workflow_depth(
    *,
    workflow_id: str,
    workflow_store: WorkflowStoreProtocol,
    iteration_store: IterationStoreProtocol,
    attempt_store: AttemptStoreProtocol,
    task_store: TaskStoreProtocol,
) -> int:
    """Number of workflow ancestors on the chain INCLUDING ``workflow_id``."""
    depth = 0
    seen_workflow_ids: set[str] = set()
    current_workflow_id = workflow_id
    while True:
        if current_workflow_id in seen_workflow_ids:
            raise TaskCenterInvariantViolation(
                "Cycle detected while resolving workflow ancestry."
            )
        seen_workflow_ids.add(current_workflow_id)
        depth += 1
        current_workflow = workflow_store.get(current_workflow_id)
        if current_workflow is None:
            raise TaskCenterInvariantViolation(f"Workflow {current_workflow_id!r} was not found.")
        if current_workflow.requested_by_task_id is None:
            return depth
        parent_task = task_store.get_task(current_workflow.requested_by_task_id)
        if parent_task is None:
            return depth
        parent_attempt_id = str(parent_task.get("task_center_attempt_id") or "")
        if not parent_attempt_id:
            return depth
        parent_attempt = attempt_store.get(parent_attempt_id)
        if parent_attempt is None:
            raise TaskCenterInvariantViolation(
                f"Parent Attempt {parent_attempt_id!r} was not found."
            )
        parent_iteration = iteration_store.get(parent_attempt.iteration_id)
        if parent_iteration is None:
            raise TaskCenterInvariantViolation(
                f"Parent Iteration {parent_attempt.iteration_id!r} was not found."
            )
        current_workflow_id = parent_iteration.workflow_id


__all__ = ["nested_workflow_depth"]
