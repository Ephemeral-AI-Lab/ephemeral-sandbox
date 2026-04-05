"""Runtime registry for agent definitions."""

from __future__ import annotations

import logging

from agents.types import AgentDefinition

logger = logging.getLogger(__name__)

# Module-level registry — unified store for built-in + user + DB agents
_DEFINITIONS: dict[str, AgentDefinition] = {}


def register_definition(defn: AgentDefinition) -> None:
    """Register or replace an agent definition at runtime."""
    _DEFINITIONS[defn.name] = defn


def unregister_definition(name: str) -> bool:
    """Remove an agent definition. Returns True if it existed."""
    return _DEFINITIONS.pop(name, None) is not None


def get_definition(name: str) -> AgentDefinition | None:
    """Look up a registered definition by name."""
    return _DEFINITIONS.get(name)


def list_definitions(source: str | None = None) -> list[AgentDefinition]:
    """List all registered definitions, optionally filtered by source."""
    defs = list(_DEFINITIONS.values())
    if source:
        defs = [d for d in defs if d.source == source]
    return defs
