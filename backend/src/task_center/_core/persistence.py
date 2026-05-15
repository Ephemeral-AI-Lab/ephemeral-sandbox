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
from task_center.episode.state import (
    Episode,
    EpisodeCreationReason,
    EpisodeStatus,
)
from task_center.mission.state import Mission, MissionStatus

# Row dicts returned by the task store. Always a serialized snapshot, never
# a live ORM row.
TaskRow = dict[str, Any]


class MissionStoreProtocol(Protocol):
    """Narrow contract for the mission persistence surface."""

    is_ready: bool

    def insert(
        self,
        *,
        task_center_run_id: str,
        requested_by_task_id: str,
        goal: str,
    ) -> Mission: ...

    def get(self, goal_id: str) -> Mission | None: ...

    def append_iteration_id(
        self, goal_id: str, iteration_id: str
    ) -> Mission: ...

    def set_status(
        self,
        goal_id: str,
        *,
        status: MissionStatus,
        final_outcome: dict[str, Any] | None,
        closed_at: datetime | None,
    ) -> Mission: ...

    def list_for_executor_task(
        self, executor_task_id: str
    ) -> list[Mission]: ...


class EpisodeStoreProtocol(Protocol):
    """Narrow contract for the episode persistence surface."""

    is_ready: bool

    def insert(
        self,
        *,
        goal_id: str,
        sequence_no: int,
        creation_reason: EpisodeCreationReason,
        goal: str,
        attempt_budget: int,
    ) -> Episode: ...

    def get(self, iteration_id: str) -> Episode | None: ...

    def append_trial_id(
        self, iteration_id: str, trial_id: str
    ) -> Episode: ...

    def set_status(
        self,
        iteration_id: str,
        *,
        status: EpisodeStatus,
        closed_at: datetime | None,
    ) -> Episode: ...

    def set_continuation_goal(
        self, iteration_id: str, *, continuation_goal: str | None
    ) -> Episode: ...

    def close_succeeded(
        self,
        iteration_id: str,
        *,
        closed_at: datetime,
        final_trial_id: str,
        continuation_goal: str | None,
    ) -> Episode: ...

    def list_for_mission(self, goal_id: str) -> list[Episode]: ...


class AttemptStoreProtocol(Protocol):
    """Narrow contract for the attempt persistence surface."""

    is_ready: bool

    def insert(
        self, *, iteration_id: str, attempt_sequence_no: int
    ) -> Attempt: ...

    def get(self, trial_id: str) -> Attempt | None: ...

    def set_stage(self, trial_id: str, stage: AttemptStage) -> Attempt: ...

    def set_planner_task_id(
        self, trial_id: str, planner_task_id: str
    ) -> Attempt: ...

    def set_generator_task_ids(
        self, trial_id: str, generator_task_ids: list[str]
    ) -> Attempt: ...

    def set_evaluator_task_id(
        self, trial_id: str, evaluator_task_id: str
    ) -> Attempt: ...

    def set_plan_contract(
        self,
        trial_id: str,
        *,
        task_specification: str,
        evaluation_criteria: list[str],
        continuation_goal: str | None,
    ) -> Attempt: ...

    def close(
        self,
        trial_id: str,
        *,
        status: AttemptStatus,
        fail_reason: AttemptFailReason | None,
        closed_at: datetime,
    ) -> Attempt: ...

    def list_for_episode(self, iteration_id: str) -> list[Attempt]: ...


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

    def create_run(
        self, *, task_center_run_id: str, request_id: str
    ) -> None: ...

    def get_run(self, task_center_run_id: str) -> TaskRow | None: ...

    def finish_run(
        self, task_center_run_id: str, *, status: str
    ) -> None: ...

    def upsert_task(
        self,
        *,
        task_id: str,
        task_center_run_id: str,
        role: str,
        rendered_prompt: str,
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

    def list_generator_tasks_for_attempt(
        self, trial_id: str
    ) -> list[TaskRow]: ...

    def set_task_status(
        self, task_id: str, *, status: str, summary: Any = ...
    ) -> TaskRow: ...

    def set_task_status_if_current(
        self,
        task_id: str,
        *,
        expected_status: str,
        status: str,
        summary: Any = ...,
    ) -> TaskRow | None: ...

    def set_task_context_packet_id(
        self, task_id: str, *, context_packet_id: str
    ) -> None: ...

    def get_evaluator_pass_summary(
        self, evaluator_task_id: str
    ) -> Any: ...


__all__ = [
    "MissionStoreProtocol",
    "EpisodeStoreProtocol",
    "AttemptStoreProtocol",
    "TaskStoreProtocol",
    "TaskRow",
]
