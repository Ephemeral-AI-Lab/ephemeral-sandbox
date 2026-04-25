"""Runtime registry for config-backed agent definitions."""

from __future__ import annotations

from agents.types import AgentDefinition

# ---------------------------------------------------------------------------
# Builtin definitions
# ---------------------------------------------------------------------------

# No repository-bundled agent names are reserved by default.
RESERVED_BUILTIN_AGENT_NAMES = frozenset()


# ---------------------------------------------------------------------------
# Registry
# ---------------------------------------------------------------------------

_DEFINITIONS: dict[str, AgentDefinition] = {}


def register_definition(defn: AgentDefinition) -> None:
    """Register or replace an agent definition at runtime."""
    _DEFINITIONS[defn.name] = defn


def unregister_definition(name: str) -> bool:
    """Remove an agent definition. Returns True if it existed."""
    return _DEFINITIONS.pop(name, None) is not None


def get_definition(name: str) -> AgentDefinition | None:
    """Look up an agent definition by name."""
    return _DEFINITIONS.get(name)


def list_definitions(source: str | None = None) -> list[AgentDefinition]:
    """List all registered definitions, optionally filtered by source."""
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
    return [d for d in _DEFINITIONS.values() if d.role == role]


def list_dispatchable_subagent_names() -> list[str]:
    """Return registered subagent names that may be targeted by run_subagent."""
    return sorted(
        defn.name
        for defn in _DEFINITIONS.values()
        if defn.agent_type == "subagent"
        and defn.dispatchable_via_run_subagent
    )
