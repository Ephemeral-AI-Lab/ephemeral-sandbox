"""AgentDefinitions used by the live e2e mock runner.

The squad uses the repository's main-profile markdown definitions so live e2e
coverage exercises the same frontmatter, variants, terminals, and system
prompts as production launches. The mock runner still executes deterministic
tool calls, but agent selection comes from real ``agent.md``-style metadata.
"""

from __future__ import annotations

import contextlib
from collections.abc import Iterator
from pathlib import Path

from agents import (
    AgentDefinition,
    list_definitions,
    load_agents_dir,
    register_definition,
    unregister_definition,
)


_MAIN_PROFILE_DIR = (
    Path(__file__).resolve().parents[2] / "agents" / "profile" / "main"
)


@contextlib.contextmanager
def registered_mock_agents() -> Iterator[None]:
    """Temporarily install the main TaskCenter squad definitions."""
    previous = list_definitions()
    for definition in previous:
        unregister_definition(definition.name)

    for definition in mock_agent_definitions():
        register_definition(definition)

    try:
        yield
    finally:
        for definition in list_definitions():
            unregister_definition(definition.name)
        for definition in previous:
            register_definition(definition)


def mock_agent_definitions() -> tuple[AgentDefinition, ...]:
    """Load the production main-profile definitions for deterministic runs."""
    return tuple(load_agents_dir(_MAIN_PROFILE_DIR))


__all__ = [
    "mock_agent_definitions",
    "registered_mock_agents",
]
