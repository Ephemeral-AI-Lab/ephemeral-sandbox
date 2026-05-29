"""Persistence Protocols at the TaskCenter boundary.

These are the narrow store contracts that ``task_center`` actually consumes.
Concrete implementations live in ``db.stores.*`` but task_center modules
depend only on these protocols, so:

- Tests can substitute in-memory or fake stores without monkey-patching
  ``db.stores`` module paths.
- The store contract can evolve independently of one implementation.
- Adding a second persistence backend (e.g. a Redis cache layer) does not
  require changes in ``task_center`` code.

Each protocol lists ONLY the methods task_center calls. Unused methods on
the concrete store classes (analytics queries, admin helpers) are out of
scope.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any, Protocol

from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.iteration.state import (
    Iteration,
    IterationCreationReason,
    IterationStatus,
)
from task_center.workflow.state import Workflow, WorkflowOrigin, WorkflowStatus

# Row dicts returned by the task store. Always a serialized snapshot, never
# a live ORM row.
TaskRow = dict[str, Any]


class WorkflowStoreProtocol(Protocol):
    """Narrow contract for the goal persistence surface."""

    is_ready: bool

    def insert(
        self,
        *,
        task_center_run_id: str,
        origin: WorkflowOrigin | None = ...,
        requested_by_task_id: str | None = ...,
        goal: str,
    ) -> Workflow: ...

    def get(self, workflow_id: str) -> Workflow | None: ...

    def append_iteration_id(self, workflow_id: str, iteration_id: str) -> Workflow: ...

    def set_status(
        self,
        workflow_id: str,
        *,
        status: WorkflowStatus,
        final_outcome: dict[str, Any] | None,
        closed_at: datetime | None,
    ) -> Workflow: ...

    def list_for_parent_task(self, parent_task_id: str) -> list[Workflow]: ...


class IterationStoreProtocol(Protocol):
    """Narrow contract for the iteration persistence surface."""

    is_ready: bool

    def insert(
        self,
        *,
        workflow_id: str,
        sequence_no: int,
        creation_reason: IterationCreationReason,
        goal: str,
        attempt_budget: int,
    ) -> Iteration: ...

    def get(self, iteration_id: str) -> Iteration | None: ...

    def append_attempt_id(self, iteration_id: str, attempt_id: str) -> Iteration: ...

    def set_status(
        self,
        iteration_id: str,
        *,
        status: IterationStatus,
        closed_at: datetime | None,
    ) -> Iteration: ...

    def set_deferred_goal_for_next_iteration(
        self, iteration_id: str, *, deferred_goal_for_next_iteration: str | None
    ) -> Iteration: ...

    def close_succeeded(
        self,
        iteration_id: str,
        *,
        plan_spec: str,
        task_summary: str,
        closed_at: datetime | None = None,
    ) -> Iteration: ...

    def list_for_workflow(self, workflow_id: str) -> list[Iteration]: ...


class AttemptStoreProtocol(Protocol):
    """Narrow contract for the attempt persistence surface."""

    is_ready: bool

    def insert(self, *, iteration_id: str, attempt_sequence_no: int) -> Attempt: ...

    def get(self, attempt_id: str) -> Attempt | None: ...

    def set_stage(self, attempt_id: str, stage: AttemptStage) -> Attempt: ...

    def set_planner_task_id(self, attempt_id: str, planner_task_id: str) -> Attempt: ...

    def set_generator_task_ids(self, attempt_id: str, generator_task_ids: list[str]) -> Attempt: ...

    def set_evaluator_task_id(self, attempt_id: str, evaluator_task_id: str) -> Attempt: ...

    def set_plan_contract(
        self,
        attempt_id: str,
        *,
        plan_spec: str,
        evaluation_criteria: list[str],
        deferred_goal_for_next_iteration: str | None,
    ) -> Attempt: ...

    def close(
        self,
        attempt_id: str,
        *,
        status: AttemptStatus,
        fail_reason: AttemptFailReason | None,
        closed_at: datetime,
    ) -> Attempt: ...

    def list_for_iteration(self, iteration_id: str) -> list[Attempt]: ...


class TaskStoreProtocol(Protocol):
    """Narrow contract for the task-center task/run persistence surface."""

    is_ready: bool

    def create_request(
        self,
        *,
        request_id: str,
        cwd: str,
        sandbox_id: str | None,
        request_prompt: str,
    ) -> None: ...

    def create_run(self, *, task_center_run_id: str, request_id: str) -> None: ...

    def get_run(self, task_center_run_id: str) -> TaskRow | None: ...

    def finish_run(self, task_center_run_id: str, *, status: str) -> None: ...

    def upsert_task(
        self,
        *,
        task_id: str,
        task_center_run_id: str,
        role: str,
        context_message: str,
        status: str,
        summaries: list[Any],
        needs: list[str],
        task_center_attempt_id: str | None,
        agent_name: str | None = ...,
        context_packet_id: str | None = ...,
        fix_target_id: str | None = ...,
        spawn_reason: str | None = ...,
    ) -> None: ...

    def get_task(self, task_id: str) -> TaskRow | None: ...

    def list_generator_tasks_for_attempt(self, attempt_id: str) -> list[TaskRow]: ...

    def set_task_status(self, task_id: str, *, status: str, summary: Any = ...) -> TaskRow: ...

    def set_task_status_if_current(
        self,
        task_id: str,
        *,
        expected_status: str,
        status: str,
        summary: Any = ...,
    ) -> TaskRow | None: ...

    def set_task_context_packet_id(self, task_id: str, *, context_packet_id: str) -> None: ...


__all__ = [
    "WorkflowStoreProtocol",
    "IterationStoreProtocol",
    "AttemptStoreProtocol",
    "TaskStoreProtocol",
    "TaskRow",
]
