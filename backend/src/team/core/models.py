"""Core team-mode dataclasses and enums."""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any, Mapping

from config.defaults import (
    DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_PLAN_SIZE,
    DEFAULT_MAX_REPLANS_PER_RUN,
    DEFAULT_MAX_TASKS,
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
# TaskSpec / TaskDefinition — agent + spec (property 1 of every Task)
# ---------------------------------------------------------------------------


def _non_blank(value: object, *, field_name: str) -> str:
    text = str(value or "").strip()
    if not text:
        raise ValueError(f"TaskSpec requires a non-empty '{field_name}'")
    return text


@dataclass(frozen=True)
class TaskSpec:
    """Structured task briefing shared by runtime tasks and submission tools."""

    goal: str
    detail: str
    acceptance_criteria: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "goal", _non_blank(self.goal, field_name="goal"))
        object.__setattr__(self, "detail", _non_blank(self.detail, field_name="detail"))
        object.__setattr__(
            self,
            "acceptance_criteria",
            _non_blank(self.acceptance_criteria, field_name="acceptance_criteria"),
        )

    @classmethod
    def from_mapping(cls, data: Mapping[str, Any]) -> "TaskSpec":
        return cls(
            goal=data.get("goal"),
            detail=data.get("detail"),
            acceptance_criteria=data.get("acceptance_criteria"),
        )

    def to_dict(self) -> dict[str, str]:
        return {
            "goal": self.goal,
            "detail": self.detail,
            "acceptance_criteria": self.acceptance_criteria,
        }


def render_task_spec(spec: TaskSpec, *, status_label: str | None = None) -> str:
    goal_header = "# Goal"
    if status_label:
        goal_header += f" {{Status: {status_label}}}"
    return "\n\n".join(
        [
            goal_header,
            spec.goal,
            "# Detail",
            spec.detail,
            "# Acceptance Criteria",
            spec.acceptance_criteria,
        ]
    )


@dataclass(init=False)
class TaskDefinition:
    """What defines a task: which agent, and what to do."""

    id: str
    spec: TaskSpec
    agent: str
    description: str = ""
    deps: list[str] = field(default_factory=list)
    scope_paths: list[str] = field(default_factory=list)
    parent_id: str | None = None

    def __init__(
        self,
        *,
        id: str,
        spec: TaskSpec | Mapping[str, Any] | None = None,
        agent: str,
        description: str = "",
        deps: list[str] | None = None,
        scope_paths: list[str] | None = None,
        parent_id: str | None = None,
    ) -> None:
        self.id = str(id)
        if isinstance(spec, TaskSpec):
            self.spec = spec
        elif isinstance(spec, Mapping):
            self.spec = TaskSpec.from_mapping(spec)
        elif spec is not None:
            raise ValueError(
                f"TaskSpec for task '{self.id}' must be an object with "
                "goal, detail, and acceptance_criteria"
            )
        else:
            raise ValueError(f"TaskDefinition '{self.id}' requires a non-empty 'spec'")
        self.agent = str(agent)
        self.description = description
        self.deps = list(deps or [])
        self.scope_paths = list(scope_paths or [])
        self.parent_id = parent_id


# ---------------------------------------------------------------------------
# Tasks
# ---------------------------------------------------------------------------


@dataclass(init=False)
class Task:
    """A task is (1) a ``TaskDefinition`` and (2) a ``TaskSubmission``.

    Non-planner agents emit a ``LeafSubmission``. Planners emit a two-stage
    ``PlannerSubmission``: stage 1 (the plan) at ``EXPANDED``, stage 2 (the
    summary) when the parent-summarizer sidecar succeeds.
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
        spec: TaskSpec | Mapping[str, Any] | None = None,
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
                spec=spec,
                agent=agent_name or "",
                description=description,
                deps=list(deps or []),
                scope_paths=list(scope_paths or []),
                parent_id=parent_id,
            )
        elif definition.parent_id is None:
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

    @property
    def description(self) -> str:
        return self.definition.description

    @property
    def deps(self) -> list[str]:
        return self.definition.deps

    @property
    def scope_paths(self) -> list[str]:
        return self.definition.scope_paths

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
    agent = str(it.get("agent") or "")
    if not task_id:
        raise ValueError("TaskDefinition requires a non-empty 'id'")
    if not agent:
        raise ValueError(f"TaskDefinition '{task_id}' requires a non-empty 'agent'")
    try:
        raw_spec = it.get("spec")
        if isinstance(raw_spec, TaskSpec):
            spec = raw_spec
        elif isinstance(raw_spec, Mapping):
            spec = TaskSpec.from_mapping(raw_spec)
        elif raw_spec is not None:
            raise ValueError(
                f"TaskSpec for task '{task_id}' must be an object with "
                "goal, detail, and acceptance_criteria"
            )
        else:
            raise ValueError(f"TaskDefinition '{task_id}' requires a non-empty 'spec'")
    except ValueError as exc:
        raise ValueError(str(exc)) from exc
    return TaskDefinition(
        id=task_id,
        spec=spec,
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


@dataclass
class LeafSubmission:
    """Submission from a non-planner agent — a single summary."""

    summary: SubmittedSummary


@dataclass
class PlannerSubmission:
    """Two-stage submission from a planner/replanner.

    Stage 1 is the plan emitted at ``EXPANDED``. Stage 2 is the summary
    copied from the parent-summarizer sidecar when all children resolve.
    """

    plan: "Plan | ReplanPlan"
    summary: SubmittedSummary | None = None


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


@dataclass
class BudgetState:
    tasks_used: int = 0
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


@dataclass
class ProjectContext:
    """Minimal project-level context for a TeamRun."""

    goal: str = ""
    user_request: str = ""
    project_key: str = ""
    repo_root: str = ""
