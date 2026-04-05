"""Agents module — first-class agent definitions, builder, and registry.

Import from here instead of deep paths:

    from agents import AgentDefinition, get_definition, AgentBuilderService
"""

from agents.types import (
    EFFORT_LEVELS,
    AgentDefinition,
)
from agents.registry import (
    get_definition,
    list_definitions,
    register_definition,
    unregister_definition,
)
from agents.loader import (
    get_agent_definition,
    get_all_agent_definitions,
    load_agents_dir,
)

__all__ = [
    "AgentDefinition",
    "EFFORT_LEVELS",
    "register_definition",
    "unregister_definition",
    "get_definition",
    "list_definitions",
    "get_agent_definition",
    "get_all_agent_definitions",
    "load_agents_dir",
]
