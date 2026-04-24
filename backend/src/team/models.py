"""Core team-mode dataclasses and enums."""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any

from config.defaults import (
    DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_NOTE_BYTES,
    DEFAULT_MAX_PLAN_SIZE,
    DEFAULT_MAX_REPLANS_PER_RUN,
    DEFAULT_MAX_TASKS,
    DEFAULT_MAX_TOTAL_NOTE_BYTES,
)


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


# ---------------------------------------------------------------------------
# Enums
# ---------------------------------------------------------------------------


class TaskStatus(str, Enum):
    PENDING = "pending"
    READY = "ready"
    RUNNING = "running"
    EXPANDED = "expanded"  # planner submitted children, waiting for them to finish
    # All children terminal; waiting for a parent-summary sidecar to finalize
    # the task. This is NOT a terminal status.
    EXPANDED_AWAITING_SUMMARY = "expanded_awaiting_summary"
    REQUEST_REPLAN = "request_replan"
    DONE = "done"
    FAILED = "failed"
    CANCELLED = "cancelled"

    @classmethod
    def of(
        cls, value: object, default: "TaskStatus | None" = None
    ) -> "TaskStatus":
        """Convert a raw value to a ``TaskStatus`` enum when possible.

        If value is already a TaskStatus, return it as-is.
        If value doesn't match any known status, return ``default``.
        """
        if default is None:
            default = cls.PENDING
        if isinstance(value, cls):
            return value
        try:
            return cls(value)
        except (TypeError, ValueError):
            return default


class TeamRunStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


TERMINAL_STATUSES: frozenset[TaskStatus] = frozenset(
    {
        TaskStatus.DONE,
        TaskStatus.FAILED,
        TaskStatus.CANCELLED,
        TaskStatus.REQUEST_REPLAN,
    }
)


# ---------------------------------------------------------------------------
# Notes
# ---------------------------------------------------------------------------


@dataclass
class Note:
    """One file-scoped entry in the Task Center."""

    id: str
    agent_name: str
    content: str
    timestamp: float = field(default_factory=time.time)
    paths: list[str] = field(default_factory=list)


# ---------------------------------------------------------------------------
# TaskDefinition — agent + spec (property 1 of every Task)
# ---------------------------------------------------------------------------


@dataclass
class TaskDefinition:
    """What defines a task: which agent, and what to do.

    ``parent_id`` is normally runtime-assigned on ``Task``. Replan validation
    also stamps it on transient child definitions before insertion so the
    expander can keep parent ownership explicit while rewriting local ids.
    """

    id: str
    objective: str
    agent: str
    description: str = ""
    deps: list[str] = field(default_factory=list)
    scope_paths: list[str] = field(default_factory=list)
    parent_id: str | None = None


# ---------------------------------------------------------------------------
# Tasks
# ---------------------------------------------------------------------------


@dataclass(init=False)
class Task:
    """A task is (1) a ``TaskDefinition`` and (2) a ``TaskSubmission``.

    Non-planner agents emit a single ``LeafSubmission``. Planners emit a
    two-stage ``PlannerSubmission``: stage 1 (the plan) at ``EXPANDED``, stage
    2 (the summary) when the parent-summarizer sidecar succeeds.

    ``submission`` is in-memory only — it is not persisted across restart.
    """

    id: str
    team_run_id: str
    definition: "TaskDefinition"
    status: TaskStatus
    submission: "TaskSubmission | None" = None
    parent_id: str | None = None
    root_id: str = ""
    depth: int = 0
    agent_run_id: str | None = None
    created_at: datetime = field(default_factory=_utcnow)
    started_at: datetime | None = None
    finished_at: datetime | None = None
    failure_reason: str | None = None
    fired_by_task_id: str | None = None

    def __init__(
        self,
        *,
        id: str,
        team_run_id: str,
        status: TaskStatus,
        definition: "TaskDefinition | None" = None,
        agent_name: str | None = None,
        objective: str | None = None,
        description: str = "",
        deps: list[str] | None = None,
        scope_paths: list[str] | None = None,
        submission: "TaskSubmission | None" = None,
        parent_id: str | None = None,
        root_id: str = "",
        depth: int = 0,
        agent_run_id: str | None = None,
        created_at: datetime | None = None,
        started_at: datetime | None = None,
        finished_at: datetime | None = None,
        failure_reason: str | None = None,
        fired_by_task_id: str | None = None,
    ) -> None:
        if definition is None:
            definition = TaskDefinition(
                id=id,
                objective=objective or "",
                agent=agent_name or "",
                description=description or "",
                deps=list(deps or []),
                scope_paths=list(scope_paths or []),
                parent_id=parent_id,
            )
        else:
            if agent_name is not None:
                definition.agent = agent_name
            if objective is not None:
                definition.objective = objective
            if description:
                definition.description = description
            if deps is not None:
                definition.deps = list(deps)
            if scope_paths is not None:
                definition.scope_paths = list(scope_paths)
            if definition.parent_id is None:
                definition.parent_id = parent_id

        self.id = id
        self.team_run_id = team_run_id
        self.definition = definition
        self.status = status
        self.submission = submission
        self.parent_id = parent_id
        self.root_id = root_id
        self.depth = depth
        self.agent_run_id = agent_run_id
        self.created_at = created_at or _utcnow()
        self.started_at = started_at
        self.finished_at = finished_at
        self.failure_reason = failure_reason
        self.fired_by_task_id = fired_by_task_id

    @property
    def agent_name(self) -> str:
        return self.definition.agent

    @agent_name.setter
    def agent_name(self, value: str) -> None:
        self.definition.agent = value

    @property
    def objective(self) -> str:
        return self.definition.objective

    @objective.setter
    def objective(self, value: str) -> None:
        self.definition.objective = value

    @property
    def description(self) -> str:
        return self.definition.description

    @description.setter
    def description(self, value: str) -> None:
        self.definition.description = value

    @property
    def deps(self) -> list[str]:
        return self.definition.deps

    @deps.setter
    def deps(self, value: list[str]) -> None:
        self.definition.deps = list(value)

    @property
    def scope_paths(self) -> list[str]:
        return self.definition.scope_paths

    @scope_paths.setter
    def scope_paths(self, value: list[str]) -> None:
        self.definition.scope_paths = list(value)

    @property
    def detached(self) -> bool:
        """True when this task no longer contributes to its parent's success.

        A task is detached once it has terminated without producing a `done`
        outcome — i.e. `failed`, `cancelled`, or `request_replan` (A is
        terminal at REQUEST_REPLAN; recovery lives on R under the parent).
        Parents treat detached children as resolved-but-skipped when deciding
        promotion (see ``fetch_expanded_parent_candidate``).
        """
        return self.status in (
            TaskStatus.FAILED,
            TaskStatus.CANCELLED,
            TaskStatus.REQUEST_REPLAN,
        )


# ---------------------------------------------------------------------------
# Plan types
# ---------------------------------------------------------------------------


@dataclass
class Plan:
    tasks: list[TaskDefinition] = field(default_factory=list)
    rationale: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Plan:
        tasks = _taskspec_list_from_field(data, field_name="tasks")
        return cls(tasks=tasks, rationale=data.get("rationale"))


@dataclass
class ReplanPlan:
    add_tasks: list[TaskDefinition] = field(default_factory=list)
    cancel_ids: list[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ReplanPlan:
        add_tasks = _taskspec_list_from_field(data, field_name="add_tasks")
        return cls(
            add_tasks=add_tasks,
            cancel_ids=list(data.get("cancel_ids") or []),
        )


def _taskspec_list_from_field(
    data: dict[str, Any],
    *,
    field_name: str,
) -> list[TaskDefinition]:
    raw_items = data.get(field_name) or []
    if not isinstance(raw_items, list):
        raise ValueError(f"'{field_name}' must be a list")

    items: list[TaskDefinition] = []
    for index, item in enumerate(raw_items):
        if not isinstance(item, dict):
            raise ValueError(f"{field_name}[{index}] must be an object")
        try:
            items.append(_taskspec_from_dict(item))
        except ValueError as exc:
            raise ValueError(f"{field_name}[{index}]: {exc}") from exc
    return items


def _taskspec_from_dict(it: dict[str, Any]) -> TaskDefinition:
    """Build a TaskDefinition from a dict, raising ValueError on missing required fields."""
    task_id = str(it.get("id") or "")
    objective = str(it.get("objective") or "")
    agent = str(it.get("agent") or "")
    if not task_id:
        raise ValueError("TaskDefinition requires a non-empty 'id'")
    if not objective:
        raise ValueError(f"TaskDefinition '{task_id}' requires a non-empty 'objective'")
    if not agent:
        raise ValueError(f"TaskDefinition '{task_id}' requires a non-empty 'agent'")
    return TaskDefinition(
        id=task_id,
        objective=objective,
        agent=agent,
        description=str(it.get("description") or ""),
        deps=list(it.get("deps") or []),
        scope_paths=list(it.get("scope_paths") or []),
        parent_id=it.get("parent_id"),
    )


# ---------------------------------------------------------------------------
# Submission types — property 2 of every Task
# ---------------------------------------------------------------------------


@dataclass
class SubmittedSummary:
    """Agent-emitted summary payload (text + optional artifact)."""

    summary: str
    artifact: dict[str, Any] | None = None
    submission_kind: str = field(default="summary", init=False, repr=False)


@dataclass
class LeafSubmission:
    """Submission from a non-planner agent — a single summary."""

    summary: SubmittedSummary
    kind: str = field(default="summary", init=False, repr=False)


@dataclass
class PlannerSubmission:
    """Two-stage submission from a planner/replanner agent.

    Stage 1 (``plan``) lands at ``EXPANDED`` when the planner emits children.
    Stage 2 (``summary``) lands when the parent-summarizer sidecar succeeds
    and its summary is copied onto the parent planner.
    """

    plan: "Plan | ReplanPlan"
    summary: SubmittedSummary | None = None
    kind: str = field(default="planner", init=False, repr=False)


TaskSubmission = LeafSubmission | PlannerSubmission


# ---------------------------------------------------------------------------
# Unified task status update — the single object handed to TaskStatusHandler
# ---------------------------------------------------------------------------


@dataclass
class TaskStatusUpdate:
    """One outcome emitted for a task — the single dispatch input to the handler.

    Exactly one of ``plan`` / ``replan`` is set for ``EXPANDED`` updates; both
    are ``None`` for every other status. ``summary`` carries the success
    summary for ``SUCCESS``, the reason for ``REQUEST_REPLAN`` / ``FAILED`` /
    ``CANCELLED``, and is ignored elsewhere.
    """

    task_id: str
    status: TaskStatus
    summary: str = ""
    plan: Plan | None = None
    replan: ReplanPlan | None = None


# ---------------------------------------------------------------------------
# Budget
# ---------------------------------------------------------------------------


@dataclass
class BudgetConfig:
    max_tasks: int = DEFAULT_MAX_TASKS
    max_depth: int = DEFAULT_MAX_DEPTH
    max_plan_size: int = DEFAULT_MAX_PLAN_SIZE
    max_replans_per_run: int = DEFAULT_MAX_REPLANS_PER_RUN
    max_note_bytes: int = DEFAULT_MAX_NOTE_BYTES
    max_total_note_bytes: int = DEFAULT_MAX_TOTAL_NOTE_BYTES


@dataclass
class BudgetState:
    tasks_used: int = 0
    note_bytes_used: int = 0
    replans_used: int = 0


# ---------------------------------------------------------------------------
# Team definition
# ---------------------------------------------------------------------------


@dataclass
class TeamDefinition:
    """Role-based team composition.

    ``entry_planner`` is the agent name used as the root task.
    ``roster`` maps canonical role names to lists of agent-definition names.
    """

    id: str
    name: str
    description: str
    entry_planner: str
    roster: dict[str, list[str]] = field(default_factory=dict)
