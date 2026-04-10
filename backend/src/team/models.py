"""Core team-mode dataclasses and enums.

Exceptions live in :mod:`team.errors`. Runtime code should import data
types from here and exception types from ``team.errors``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any

_OWNED_FAILURES_CAP = 64


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


def _dedupe_str_list(values: Any) -> list[str] | None:
    if not isinstance(values, list):
        return None
    seen: set[str] = set()
    deduped: list[str] = []
    for raw in values:
        if not isinstance(raw, str):
            continue
        item = raw.strip()
        if not item or item in seen:
            continue
        seen.add(item)
        deduped.append(item)
    return deduped


def _normalize_payload(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict):
        return {}
    normalized = dict(payload)
    for key in ("owned_files", "verify"):
        deduped = _dedupe_str_list(normalized.get(key))
        if deduped is not None:
            normalized[key] = deduped
    deduped_failures = _dedupe_str_list(normalized.get("owned_failures"))
    if deduped_failures is not None:
        raw_total = len([item for item in normalized.get("owned_failures") or [] if isinstance(item, str) and item.strip()])
        if raw_total > len(deduped_failures):
            normalized["owned_failures_total"] = raw_total
        if len(deduped_failures) > _OWNED_FAILURES_CAP:
            normalized["owned_failures_unique_total"] = len(deduped_failures)
            normalized["owned_failures"] = deduped_failures[:_OWNED_FAILURES_CAP]
        else:
            normalized["owned_failures"] = deduped_failures
    return normalized


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
class Briefing:
    """Parent→child context handoff. Wraps a brief (artifact) or inline text."""

    name: str
    source: str  # "artifact" | "inline"
    ref: str | None = None
    inline: str | None = None
    description: str | None = None

    def __post_init__(self) -> None:
        if self.source not in ("artifact", "inline"):
            raise ValueError(f"Briefing.source must be 'artifact' or 'inline', got {self.source!r}")
        if not self.name:
            raise ValueError("Briefing.name must be non-empty")
        if self.source == "artifact":
            if not self.ref or self.inline is not None:
                raise ValueError("Briefing(source='artifact') requires ref and forbids inline")
        else:
            if not self.inline or self.ref is not None:
                raise ValueError("Briefing(source='inline') requires inline and forbids ref")


@dataclass
class DependencyArtifact:
    """Run-scoped frozen snapshot of a completed dep's artifact (for rendering)."""

    source_wi_id: str
    artifact_ref: str
    display_name: str | None = None


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
    local_id: str | None = None
    briefings: list[Briefing] = field(default_factory=list)
    dep_artifacts: list[DependencyArtifact] = field(default_factory=list)
    retry_count: int = 0
    max_retries: int = 2
    replan_source_id: str | None = None
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
    briefings: list[Briefing] = field(default_factory=list)


@dataclass
class Plan:
    items: list[WorkItemSpec] = field(default_factory=list)
    rationale: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Plan":
        items = [
            WorkItemSpec(
                agent_name=str(it["agent_name"]),
                payload=_normalize_payload(it.get("payload") or {}),
                local_id=it.get("local_id"),
                deps=list(it.get("deps") or []),
                notes=it.get("notes"),
                timeout_seconds=it.get("timeout_seconds"),
                kind=WorkItemKind(it.get("kind", "atomic")),
                briefings=[Briefing(**b) for b in (it.get("briefings") or [])],
            )
            for it in (data.get("items") or [])
        ]
        return cls(items=items, rationale=data.get("rationale"))


@dataclass
class RetryRequest:
    """Posthook decision: retry the current work item."""

    reason: str
    retry_count: int = 0
    max_retries: int = 2


@dataclass
class ReplanRequest:
    """Posthook decision: replan at the current node level."""

    reason: str
    context: str
    suggestion: str = ""


@dataclass
class ReplanItemSpec:
    """Specification for a corrective work item added by the replanner."""

    agent_name: str
    payload: dict[str, Any] = field(default_factory=dict)
    local_id: str | None = None
    deps: list[str] = field(default_factory=list)
    notes: str | None = None
    timeout_seconds: float | None = None
    kind: WorkItemKind = WorkItemKind.ATOMIC
    briefings: list[Briefing] = field(default_factory=list)


@dataclass
class ReplanPlan:
    """Validated replan output: items to add and items to cancel."""

    add_items: list[ReplanItemSpec] = field(default_factory=list)
    cancel_ids: list[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ReplanPlan":
        add_items = [
            ReplanItemSpec(
                agent_name=str(it["agent_name"]),
                payload=_normalize_payload(it.get("payload") or {}),
                local_id=it.get("local_id"),
                deps=list(it.get("deps") or []),
                notes=it.get("notes"),
                timeout_seconds=it.get("timeout_seconds"),
                kind=WorkItemKind(it.get("kind", "atomic")),
                briefings=[Briefing(**b) for b in (it.get("briefings") or [])],
            )
            for it in (data.get("add_items") or [])
        ]
        return cls(
            add_items=add_items,
            cancel_ids=list(data.get("cancel_ids") or []),
        )


@dataclass
class AgentResult:
    """Return shape the Worker reconstructs from a finished run_query call."""

    artifact: Any
    summary: str
    submitted_plan: Plan | None = None
    submitted_replan: ReplanPlan | None = None


@dataclass
class BudgetConfig:
    max_work_items: int = 200
    max_depth: int = 5
    max_plan_size: int = 50
    max_artifact_bytes: int = 1_000_000
    max_total_artifact_bytes: int = 50_000_000
    default_work_item_timeout: float | None = None
    max_briefing_bytes: int = 32_000
    max_shared_briefings: int = 16
    max_retries_per_item: int = 2
    max_replans_per_run: int = 5


@dataclass
class BudgetState:
    work_items_used: int = 0
    artifact_bytes_used: int = 0
    replans_used: int = 0


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
