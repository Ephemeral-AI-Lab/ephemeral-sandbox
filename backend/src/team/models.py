"""Core team-mode dataclasses and enums.

Simplified per Plan A: Task Center replaces briefings, Note replaces
Briefing + DependencyArtifact, TaskSpec replaces the former WorkItemSpec.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any, Literal

from config.defaults import (
    DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_NOTE_BYTES,
    DEFAULT_MAX_PLAN_SIZE,
    DEFAULT_MAX_REPLANS_PER_RUN,
    DEFAULT_MAX_RETRIES_PER_ITEM,
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
    PAUSED = "paused"
    REPLANNING = "replanning"
    DONE = "done"
    FAILED = "failed"
    CANCELLED = "cancelled"


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
# Note — the only context primitive (replaces Briefing + DependencyArtifact)
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
# TaskSpec — what the planner submits
# ---------------------------------------------------------------------------


CascadePolicy = Literal["cancel", "retry_first", "continue"]


@dataclass
class TaskSpec:
    """One item in a plan."""

    id: str
    task: str
    agent: str
    deps: list[str] = field(default_factory=list)
    scope_paths: list[str] = field(default_factory=list)
    cascade_policy: CascadePolicy = "cancel"


# ---------------------------------------------------------------------------
# Task — runtime execution unit (replaces WorkItem)
# ---------------------------------------------------------------------------


@dataclass
class Task:
    id: str
    team_run_id: str
    agent_name: str
    status: TaskStatus
    task: str
    deps: list[str] = field(default_factory=list)
    scope_paths: list[str] = field(default_factory=list)
    cascade_policy: CascadePolicy = "cancel"
    parent_id: str | None = None
    root_id: str = ""
    depth: int = 0
    pending_dep_count: int = 0
    retry_count: int = 0
    max_retries: int = 2
    agent_run_id: str | None = None
    created_at: datetime = field(default_factory=_utcnow)
    started_at: datetime | None = None
    finished_at: datetime | None = None
    failure_reason: str | None = None
    blocker_id: str | None = None
    fired_by_task_id: str | None = None
    pause_checkpoint: str | None = None
    pause_verdict: str | None = None


# ---------------------------------------------------------------------------
# Plan types
# ---------------------------------------------------------------------------


@dataclass
class Plan:
    tasks: list[TaskSpec] = field(default_factory=list)
    rationale: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Plan:
        tasks = _taskspec_list_from_field(data, field_name="tasks")
        return cls(tasks=tasks, rationale=data.get("rationale"))


@dataclass
class ReplanPlan:
    add_tasks: list[TaskSpec] = field(default_factory=list)
    cancel_ids: list[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ReplanPlan:
        add_tasks = _taskspec_list_from_field(data, field_name="add_tasks")
        return cls(
            add_tasks=add_tasks,
            cancel_ids=list(data.get("cancel_ids") or []),
        )


_VALID_CASCADE_POLICIES: frozenset[str] = frozenset({"cancel", "retry_first", "continue"})


def _taskspec_list_from_field(
    data: dict[str, Any],
    *,
    field_name: str,
) -> list[TaskSpec]:
    raw_items = data.get(field_name) or []
    if not isinstance(raw_items, list):
        raise ValueError(f"'{field_name}' must be a list")

    items: list[TaskSpec] = []
    for index, item in enumerate(raw_items):
        if not isinstance(item, dict):
            raise ValueError(f"{field_name}[{index}] must be an object")
        try:
            items.append(_taskspec_from_dict(item))
        except ValueError as exc:
            raise ValueError(f"{field_name}[{index}]: {exc}") from exc
    return items


def _taskspec_from_dict(it: dict[str, Any]) -> TaskSpec:
    """Build a TaskSpec from a dict, raising ValueError on missing required fields."""
    task_id = str(it.get("id") or "")
    task_text = str(it.get("task") or "")
    agent = str(it.get("agent") or "")
    if not task_id:
        raise ValueError("TaskSpec requires a non-empty 'id'")
    if not task_text:
        raise ValueError(f"TaskSpec '{task_id}' requires a non-empty 'task'")
    if not agent:
        raise ValueError(f"TaskSpec '{task_id}' requires a non-empty 'agent'")
    raw_policy = str(it.get("cascade_policy", "cancel"))
    cascade_policy: CascadePolicy = (
        raw_policy if raw_policy in _VALID_CASCADE_POLICIES else "cancel"
    )  # type: ignore[assignment]
    return TaskSpec(
        id=task_id,
        task=task_text,
        agent=agent,
        deps=list(it.get("deps") or []),
        scope_paths=list(it.get("scope_paths") or []),
        cascade_policy=cascade_policy,
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
class RetryRequest:
    reason: str
    submission_kind: str = field(default="retry", init=False, repr=False)


@dataclass
class ReplanRequest:
    reason: str
    suggestion: str | None = None
    submission_kind: str = field(default="replan", init=False, repr=False)


@dataclass
class BlockerDeclaration:
    root_cause_paths: list[str]
    reason: str
    suggestion: str | None = None
    submission_kind: str = field(default="blocker", init=False, repr=False)


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
    max_retries_per_item: int = DEFAULT_MAX_RETRIES_PER_ITEM
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


# ---------------------------------------------------------------------------
# Blocker protocol
# ---------------------------------------------------------------------------


class BlockerStatus(str, Enum):
    ASSESSING = "assessing"
    FIXING = "fixing"
    RESOLVED = "resolved"
    FAILED = "failed"


@dataclass
class Blocker:
    id: str
    team_run_id: str
    status: BlockerStatus
    reason: str
    root_cause_paths: list[str]
    initiating_task_id: str
    fix_task_id: str | None = None
    declared_by: str | None = None
    fix_summary: str | None = None
    pending_assessments: int = 0
    created_at: float = field(default_factory=time.time)
    resolved_at: float | None = None
