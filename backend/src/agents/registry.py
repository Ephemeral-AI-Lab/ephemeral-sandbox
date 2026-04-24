"""Runtime registry for config-backed agent definitions."""

from __future__ import annotations

import logging

from agents.types import AgentDefinition

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Builtin definitions
# ---------------------------------------------------------------------------

# Names reserved for builtins loaded from ``backend/config/agents``.
RESERVED_BUILTIN_AGENT_NAMES = frozenset(
    {
        "root_planner",
        "team_planner",
        "developer",
        "validator",
        "scout",
        "team_replanner",
        "parent_summarizer",
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
    """Look up an agent definition by name."""
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
            existing = _DEFINITIONS.get(defn.name)
            if existing is not None and existing.source == "builtin":
                continue
            _DEFINITIONS[defn.name] = defn
    except Exception:
        logger.debug("Failed to load external agent definitions", exc_info=True)
