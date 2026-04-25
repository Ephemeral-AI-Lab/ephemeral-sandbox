"""Resolve CI attribution metadata from a :class:`ToolExecutionContext`.

Tools that dispatch through the code-intelligence service (``svc.cmd``,
``svc.edit_file``, etc.) need agent / team / task identifiers so the
arbiter ledger records who did what. Those four fields are read from the
same metadata slots in every caller, so the extraction lives here.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from tools.core.base import ToolExecutionContext

__all__ = [
    "AgentAttribution",
    "agent_attribution_from_context",
    "rebind_ci_service",
    "resolved_agent_id",
]


def rebind_ci_service(context: ToolExecutionContext, svc: Any) -> None:
    """Point *svc* at the sandbox on *context* before a sync OCC call.

    Typed ``svc.*`` APIs run sync inside a worker thread; they read through
    :class:`ContentManager`, which speaks to whatever sandbox the service is
    currently bound to. When a tool holds a newer sandbox than the service
    (e.g. after ``_recover_sandbox`` reattaches), reads must not go through
    the stale handle — rebind first so the sync path sees the current one.

    No-op when the context has no sandbox or *svc* cannot be rebound.
    """
    sandbox = context.metadata.get("ci_sandbox") or context.metadata.get("daytona_sandbox")
    rebind = getattr(svc, "rebind_sandbox", None)
    if sandbox is None or not callable(rebind):
        return
    rebind(sandbox)


@dataclass(frozen=True)
class AgentAttribution:
    """Identifiers the service needs to attribute a mutation to an actor."""

    agent_id: str
    run_id: str
    agent_run_id: str
    task_id: str


def resolved_agent_id(context: ToolExecutionContext, *, preferred: str = "") -> str:
    """Return a non-empty actor label for ledger attribution.

    Priority: caller-supplied *preferred* → ``agent_run_id`` → ``agent_name``.
    The run-id-first order matches what every mutation tool used before
    this helper existed: it keeps the arbiter ledger keyed on a
    unique-per-run identifier so cross-run analysis doesn't collide two
    different runs of the same named role. Consumers that want the human
    label read ``agent_name`` directly.

    Returns ``""`` when the context lacks any actor identity — callers
    downstream should treat that as "anonymous".
    """
    explicit = str(preferred or "").strip()
    if explicit:
        return explicit
    agent_run_id = str(context.metadata.get("agent_run_id") or "").strip()
    if agent_run_id:
        return agent_run_id
    return str(context.metadata.get("agent_name") or "").strip()


def agent_attribution_from_context(
    context: ToolExecutionContext,
    *,
    preferred_agent_id: str = "",
) -> AgentAttribution:
    """Build an :class:`AgentAttribution` from a tool execution context."""
    return AgentAttribution(
        agent_id=resolved_agent_id(context, preferred=preferred_agent_id),
        run_id=str(context.metadata.get("run_id") or ""),
        agent_run_id=str(context.metadata.get("agent_run_id") or ""),
        task_id=str(context.metadata.get("work_item_id") or ""),
    )
