"""Runtime registry for agent definitions.

Holds builtin, user-supplied (loaded from disk), and plugin agent definitions
in a single in-memory map. Builtins are seeded at import time; user/plugin
agents are loaded lazily on first lookup (and can be reloaded explicitly).
"""

from __future__ import annotations

import logging

from agents.types import AgentDefinition

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Builtin definitions
# ---------------------------------------------------------------------------

# Names reserved for agents seeded from the database by
# ``AgentBuilderService.load_all_from_db()``.  External (user/plugin)
# agent definitions are blocked from claiming these names so that the
# DB-seeded builtins are never shadowed.  The definitions themselves are
# *not* registered here — see ``AgentBuilderService`` for the
# authoritative seed path.
RESERVED_BUILTIN_AGENT_NAMES = frozenset(
    {
        "team_planner",
        "developer",
        "validator",
        "scout",
        "resolver",
        "team_replanner",
        "submit_plan_agent",
        "decision_submit_retry",
        "decision_submit_replan",
    }
)


# ---------------------------------------------------------------------------
# Registry
# ---------------------------------------------------------------------------

_DEFINITIONS: dict[str, AgentDefinition] = {}
_external_loaded = False


def register_definition(defn: AgentDefinition) -> None:
    """Register or replace an agent definition at runtime."""
    _DEFINITIONS[defn.name] = defn


def unregister_definition(name: str) -> bool:
    """Remove an agent definition. Returns True if it existed."""
    return _DEFINITIONS.pop(name, None) is not None


def get_definition(name: str) -> AgentDefinition | None:
    """Look up an agent definition by name (loads user/plugin agents lazily)."""
    _ensure_external_loaded()
    return _DEFINITIONS.get(name)


def list_definitions(source: str | None = None) -> list[AgentDefinition]:
    """List all registered definitions, optionally filtered by source."""
    _ensure_external_loaded()
    defs = list(_DEFINITIONS.values())
    if source:
        defs = [d for d in defs if d.source == source]
    return defs


def get_role(agent_name: str) -> str | None:
    """Return the ``role`` tag for *agent_name*, or ``None``."""
    defn = get_definition(agent_name)
    return defn.role if defn is not None else None


def has_role(agent_name: str, role: str) -> bool:
    """Check whether *agent_name* is registered with the given *role*."""
    return get_role(agent_name) == role


def find_by_role(role: str) -> list[AgentDefinition]:
    """Return all registered definitions whose ``role`` matches."""
    _ensure_external_loaded()
    return [d for d in _DEFINITIONS.values() if d.role == role]


def list_dispatchable_subagent_names() -> list[str]:
    """Return registered subagent names that may be targeted by run_subagent."""
    _ensure_external_loaded()
    return sorted(
        defn.name
        for defn in _DEFINITIONS.values()
        if defn.agent_type == "subagent"
        and defn.dispatchable_via_run_subagent
    )


def _ensure_external_loaded() -> None:
    global _external_loaded
    if _external_loaded:
        return
    _external_loaded = True  # set first to avoid recursion on failure
    try:
        from agents.loader import load_external_agents

        for defn in load_external_agents():
            if defn.name in RESERVED_BUILTIN_AGENT_NAMES:
                logger.warning(
                    "Ignoring external agent definition %r because the name is reserved for a builtin agent",
                    defn.name,
                )
                continue
            # External definitions may replace earlier external definitions,
            # but never a builtin reserved name.
            existing = _DEFINITIONS.get(defn.name)
            if existing is not None and existing.source != "builtin":
                continue
            _DEFINITIONS[defn.name] = defn
    except Exception:
        logger.debug("Failed to load external agent definitions", exc_info=True)
