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

SUBAGENT_NAME = "subagent"

_SUBAGENT_SYSTEM_PROMPT = """You are a focused worker subagent handling one delegated task.

Return the result as plain text in your final assistant message; the parent only sees that final message.
Stay within scope. Do not ask clarifying questions. If something is ambiguous, make a reasonable choice and mention it briefly in the final answer.
Follow literal output, marker, and formatting requirements exactly.
Default to answering from the delegated prompt itself. Do not inspect the workspace or call tools unless the prompt explicitly requires external information or file changes.
Do not spawn subagents or launch background tasks.
When the task is done, stop and give a concise final result."""


def _builtin_definitions() -> list[AgentDefinition]:
    return [
        AgentDefinition(
            name=SUBAGENT_NAME,
            description=(
                "Focused worker subagent spawned by parent agents via run_subagent "
                "to complete one delegated task in isolation."
            ),
            system_prompt=_SUBAGENT_SYSTEM_PROMPT,
            model="inherit",
            max_turns=15,
            toolkits=["sandbox_operations", "code_intelligence"],
            agent_type="subagent",
            source="builtin",
        ),
    ]


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


def _ensure_external_loaded() -> None:
    global _external_loaded
    if _external_loaded:
        return
    _external_loaded = True  # set first to avoid recursion on failure
    try:
        from agents.loader import load_external_agents

        for defn in load_external_agents():
            # Don't overwrite a builtin with itself; user/plugin defs take
            # precedence over builtins of the same name.
            existing = _DEFINITIONS.get(defn.name)
            if existing is not None and existing.source != "builtin":
                continue
            _DEFINITIONS[defn.name] = defn
    except Exception:
        logger.debug("Failed to load external agent definitions", exc_info=True)


# Seed builtins at import time.
for _defn in _builtin_definitions():
    _DEFINITIONS.setdefault(_defn.name, _defn)
