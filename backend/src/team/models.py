"""Core team-mode dataclasses and enums.

Exceptions live in :mod:`team.errors`. Runtime code should import data
types from here and exception types from ``team.errors``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


class WorkItemKind(str, Enum):
    ATOMIC = "atomic"
    EXPANDABLE = "expandable"


class WorkItemStatus(str, Enum):
    PENDING = "pending"
    READY = "ready"
    RUNNING = "running"
    DONE = "done"
    FAILED = "failed"
    CANCELLED = "cancelled"


class TeamRunStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"


TERMINAL_WI_STATUSES: frozenset[WorkItemStatus] = frozenset(
    {WorkItemStatus.DONE, WorkItemStatus.FAILED, WorkItemStatus.CANCELLED}
)


@dataclass
class WorkItem:
    id: str
    team_run_id: str
    agent_name: str
    status: WorkItemStatus
    kind: WorkItemKind = WorkItemKind.ATOMIC
    deps: list[str] = field(default_factory=list)
    parent_id: str | None = None
    root_id: str = ""
    agent_run_id: str | None = None
    payload: dict[str, Any] = field(default_factory=dict)
    artifact_ref: str | None = None
    timeout_seconds: float | None = None
    depth: int = 0
    created_at: datetime = field(default_factory=_utcnow)
    started_at: datetime | None = None
    finished_at: datetime | None = None
    failure_reason: str | None = None


@dataclass
class WorkItemSpec:
    agent_name: str
    payload: dict[str, Any] = field(default_factory=dict)
    local_id: str | None = None
    deps: list[str] = field(default_factory=list)
    notes: str | None = None
    timeout_seconds: float | None = None
    kind: WorkItemKind = WorkItemKind.ATOMIC


@dataclass
class Plan:
    items: list[WorkItemSpec] = field(default_factory=list)
    rationale: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Plan":
        items = [
            WorkItemSpec(
                agent_name=str(it["agent_name"]),
                payload=dict(it.get("payload") or {}),
                local_id=it.get("local_id"),
                deps=list(it.get("deps") or []),
                notes=it.get("notes"),
                timeout_seconds=it.get("timeout_seconds"),
                kind=WorkItemKind(it.get("kind", "atomic")),
            )
            for it in (data.get("items") or [])
        ]
        return cls(items=items, rationale=data.get("rationale"))


@dataclass
class AgentResult:
    """Return shape the Worker reconstructs from a finished run_query call."""

    artifact: Any
    summary: str
    submitted_plan: Plan | None = None


@dataclass
class BudgetConfig:
    max_work_items: int = 200
    max_depth: int = 5
    max_plan_size: int = 50
    max_artifact_bytes: int = 1_000_000
    max_total_artifact_bytes: int = 50_000_000
    default_work_item_timeout: float = 300.0


@dataclass
class BudgetState:
    work_items_used: int = 0
    artifact_bytes_used: int = 0


@dataclass
class TeamDefinition:
    """Composition blob naming which agent plays which role in a team run.

    A ``TeamDefinition`` is persistent metadata (stored in ``team/db/``) that
    selects the planner agent and records the intended worker pool for a
    team. ``planner_agent`` and ``worker_agents`` are name references looked
    up in ``agents.registry`` at team-run start time; broken references fail
    fast with a clear error. ``worker_agents`` is advisory metadata — the
    planner is responsible for picking agents for its emitted WorkItemSpecs
    and may pick any registered agent. Enforcing a hard whitelist at the
    Dispatcher is future work.
    """

    id: str
    name: str
    description: str
    planner_agent: str
    worker_agents: list[str] = field(default_factory=list)
