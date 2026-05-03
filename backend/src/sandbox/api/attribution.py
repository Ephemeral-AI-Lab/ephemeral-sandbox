"""Actor identity for sandbox API calls.

``AgentAttribution`` predates the new ``RequestActor`` model and is
preserved here because the engine entry points in
:mod:`sandbox.lifecycle.commit` consume its fields by name. New
call sites should accept ``RequestActor`` and translate via
:func:`actor_from_attribution` / :func:`attribution_from_actor`.

This module is provider-neutral: it must not import from
``sandbox.daytona``, ``sandbox.runtime``, or ``tools.*``.
"""

from __future__ import annotations

from dataclasses import dataclass

from sandbox.api.models import RequestActor

__all__ = [
    "AgentAttribution",
    "actor_from_attribution",
    "attribution_from_actor",
    "build_actor",
]


@dataclass(frozen=True)
class AgentAttribution:
    """Identifiers the OCC engine needs to attribute a mutation to an actor."""

    agent_id: str
    run_id: str
    agent_run_id: str
    task_id: str


def actor_from_attribution(attribution: AgentAttribution) -> RequestActor:
    """Lift an :class:`AgentAttribution` into a :class:`RequestActor`."""
    return RequestActor(
        agent_id=attribution.agent_id,
        run_id=attribution.run_id,
        agent_run_id=attribution.agent_run_id,
        task_id=attribution.task_id,
    )


def attribution_from_actor(actor: RequestActor) -> AgentAttribution:
    """Project a :class:`RequestActor` back into the engine-side attribution shape."""
    return AgentAttribution(
        agent_id=actor.agent_id,
        run_id=actor.run_id,
        agent_run_id=actor.agent_run_id,
        task_id=actor.task_id,
    )


def build_actor(
    *,
    agent_id: str,
    run_id: str = "",
    agent_run_id: str = "",
    task_id: str = "",
    preferred_agent_id: str = "",
) -> RequestActor:
    """Construct a :class:`RequestActor` with the legacy ``agent_id`` priority.

    Priority for the resolved ``agent_id`` mirrors the prior
    ``resolved_agent_id`` helper exactly: caller-supplied
    ``preferred_agent_id`` → ``agent_run_id`` → ``agent_id``. The
    arbiter ledger keys on this resolved id, so preserving the priority
    keeps cross-run analysis stable across the refactor.
    """
    explicit = str(preferred_agent_id or "").strip()
    if explicit:
        resolved = explicit
    else:
        resolved = str(agent_run_id or "").strip() or str(agent_id or "").strip()
    return RequestActor(
        agent_id=resolved,
        run_id=str(run_id or ""),
        agent_run_id=str(agent_run_id or ""),
        task_id=str(task_id or ""),
    )
