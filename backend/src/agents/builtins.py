"""Built-in agent definitions.

Previously contained 7 hardcoded agents.  These have been replaced by
DB-seeded specialists imported from the SuperCocoa agent directory.
See :mod:`ephemeralos.agents.seed` for the migration logic.
"""

from __future__ import annotations

from ephemeralos.agents.types import AgentDefinition

# ---------------------------------------------------------------------------
# Built-in agent definitions — now empty (specialists live in the database)
# ---------------------------------------------------------------------------

_BUILTIN_AGENTS: list[AgentDefinition] = []


def get_builtin_agent_definitions() -> list[AgentDefinition]:
    """Return the built-in agent definitions (empty — see DB seed)."""
    return list(_BUILTIN_AGENTS)
