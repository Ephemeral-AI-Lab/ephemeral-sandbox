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
    {TaskStatus.DONE, TaskStatus.FAILED, TaskStatus.CANCELLED}
)


# ---------------------------------------------------------------------------
# Note tags — controlled vocabulary for note classification
# ---------------------------------------------------------------------------


class NoteTag(str, Enum):
    DISCOVERY = "discovery"
    IMPLEMENTATION = "implementation"
    BUG_FIX = "bug_fix"
    BLOCKER = "blocker"
    PROPOSAL = "proposal"
    VERIFICATION = "verification"
    ARCHITECTURE = "architecture"
    DEPENDENCY = "dependency"
    WARNING = "warning"
    REFACTOR = "refactor"


# ---------------------------------------------------------------------------
# Notes
# ---------------------------------------------------------------------------


@dataclass
class Note:
    """One entry in the Task Center."""

    id: str
    task_id: str
    agent_name: str
    content: str
    timestamp: float = field(default_factory=time.time)
    paths: list[str] = field(default_factory=list)
    tags: list[str] = field(default_factory=list)
    parent_note_id: str | None = None


# ---------------------------------------------------------------------------
# TaskDefinition — what the planner submits
# ---------------------------------------------------------------------------


@dataclass
class TaskDefinition:
    """One item in a plan."""

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


@dataclass
class Task:
    id: str
    team_run_id: str
    agent_name: str
    status: TaskStatus
    objective: str
    description: str = ""
    deps: list[str] = field(default_factory=list)
    scope_paths: list[str] = field(default_factory=list)
    parent_id: str | None = None
    root_id: str = ""
    depth: int = 0
    agent_run_id: str | None = None
    created_at: datetime = field(default_factory=_utcnow)
    started_at: datetime | None = None
    finished_at: datetime | None = None
    failure_reason: str | None = None
    fired_by_task_id: str | None = None

    @property
    def detached(self) -> bool:
        """True when this task no longer contributes to its parent's success.

        A task is detached once it has terminated without producing a `done`
        outcome — i.e. `failed` or `cancelled`. Parents treat detached children
        as resolved-but-skipped when deciding promotion (see
        ``fetch_expanded_parent_candidate``).
        """
        return self.status in (TaskStatus.FAILED, TaskStatus.CANCELLED)


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
    parent_id = it.get("parent_id")
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
        parent_id=str(parent_id) if parent_id is not None else None,
    )


# ---------------------------------------------------------------------------
# Submission types
# ---------------------------------------------------------------------------


@dataclass
class SubmittedSummary:
    summary: str
    artifact: dict[str, Any] | None = None
    submission_kind: str = field(default="summary", init=False, repr=False)


@dataclass
class ReplanRequest:
    reason: str
    suggestion: str | None = None
    submission_kind: str = field(default="replan", init=False, repr=False)


# ---------------------------------------------------------------------------
# Result type for executor dispatch
# ---------------------------------------------------------------------------


@dataclass
class AgentResult:
    summary: str
    submitted_plan: Plan | None = None
    submitted_replan: ReplanPlan | None = None


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
    terminal_tools: dict[str, set[str]] = field(default_factory=dict)
