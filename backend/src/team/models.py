"""Core team-mode dataclasses and enums.

Exceptions live in :mod:`team.errors`. Runtime code should import data
types from here and exception types from ``team.errors``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any

from config.defaults import (
    DEFAULT_MAX_WORK_ITEMS,
    DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_PLAN_SIZE,
    DEFAULT_MAX_VALIDATORS_PER_PLAN,
    DEFAULT_REQUIRE_VALIDATOR_FOR_PLAN_SIZE,
    DEFAULT_MAX_ARTIFACT_BYTES,
    DEFAULT_MAX_TOTAL_ARTIFACT_BYTES,
    DEFAULT_WORK_ITEM_TIMEOUT,
    DEFAULT_MAX_BRIEFING_BYTES,
    DEFAULT_MAX_SHARED_BRIEFINGS,
    DEFAULT_MAX_RETRIES_PER_ITEM,
    DEFAULT_MAX_REPLANS_PER_RUN,
)


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
    """Deduplicate well-known string-list payload fields.

    This is intentionally generic — benchmark-specific truncation (e.g.
    capping ``owned_failures`` to a preview limit) belongs in the
    benchmark layer, not here.
    """
    if not isinstance(payload, dict):
        return {}
    normalized = dict(payload)
    for key in ("owned_files", "owned_failures", "verify"):
        deduped = _dedupe_str_list(normalized.get(key))
        if deduped is not None:
            normalized[key] = deduped
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
    replace_failed_validator: bool = False

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
            replace_failed_validator=bool(data.get("replace_failed_validator", False)),
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
    max_work_items: int = DEFAULT_MAX_WORK_ITEMS
    max_depth: int = DEFAULT_MAX_DEPTH
    max_plan_size: int = DEFAULT_MAX_PLAN_SIZE
    max_validators_per_plan: int | None = DEFAULT_MAX_VALIDATORS_PER_PLAN
    require_validator_for_plan_size: int | None = DEFAULT_REQUIRE_VALIDATOR_FOR_PLAN_SIZE
    max_artifact_bytes: int = DEFAULT_MAX_ARTIFACT_BYTES
    max_total_artifact_bytes: int = DEFAULT_MAX_TOTAL_ARTIFACT_BYTES
    default_work_item_timeout: float | None = DEFAULT_WORK_ITEM_TIMEOUT
    max_briefing_bytes: int = DEFAULT_MAX_BRIEFING_BYTES
    max_shared_briefings: int = DEFAULT_MAX_SHARED_BRIEFINGS
    max_retries_per_item: int = DEFAULT_MAX_RETRIES_PER_ITEM
    max_replans_per_run: int = DEFAULT_MAX_REPLANS_PER_RUN


@dataclass
class BudgetState:
    work_items_used: int = 0
    artifact_bytes_used: int = 0
    replans_used: int = 0


@dataclass
class TeamDefinition:
    """Composition blob naming which agents fill which slots in a team run.

    ``roster`` maps freeform slot labels to agent-definition names looked
    up in ``agents.registry`` at team-run start time.  At least one entry
    must resolve to an agent whose ``role == "planner"``; beyond that any
    combination is valid.
    """

    id: str
    name: str
    description: str
    roster: dict[str, str] = field(default_factory=dict)

    # Back-compat helpers for code that still uses the old fields.
    @property
    def planner_agent(self) -> str:
        from agents.registry import has_role
        for agent_name in self.roster.values():
            if has_role(agent_name, "planner"):
                return agent_name
        return next(iter(self.roster.values()), "")

    @property
    def worker_agents(self) -> list[str]:
        from agents.registry import has_role
        return [n for n in self.roster.values() if not has_role(n, "planner")]
