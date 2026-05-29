"""Dependency-light audit event primitives shared by domain packages."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import Any, Literal, Protocol

JsonValue = Any
AuditSource = Literal["task_center", "engine", "sandbox", "live_e2e"]


@dataclass(frozen=True, slots=True)
class AuditNode:
    """Correlation envelope for audit events.

    Producers populate the identifiers they already know. Collectors should not
    infer missing identifiers from unrelated payload text.
    """

    task_center_run_id: str | None = None
    request_id: str | None = None
    workflow_id: str | None = None
    iteration_id: str | None = None
    attempt_id: str | None = None
    task_center_task_id: str | None = None
    agent_name: str | None = None
    agent_run_id: str | None = None
    sandbox_id: str | None = None
    tool_name: str | None = None
    tool_use_id: str | None = None


@dataclass(frozen=True, slots=True)
class AuditEvent:
    """Structured audit event emitted by a behavior-owning package."""

    source: AuditSource
    type: str
    node: AuditNode
    payload: Mapping[str, JsonValue] = field(default_factory=dict)
    correlation_id: str | None = None
    ts: datetime = field(default_factory=lambda: datetime.now(UTC))


class AuditSink(Protocol):
    """Write-only audit side channel."""

    def publish(self, event: AuditEvent) -> None: ...


class NoopAuditSink:
    """Audit sink used when collection is disabled."""

    def publish(self, event: AuditEvent) -> None:
        return None


__all__ = [
    "AuditEvent",
    "AuditNode",
    "AuditSink",
    "AuditSource",
    "JsonValue",
    "NoopAuditSink",
]
