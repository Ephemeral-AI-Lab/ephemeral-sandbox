"""Per-agent ``run_subagent`` payload validators.

``run_subagent`` stays agent-agnostic: it looks up a validator by the
target agent's name and, if present, asks it to vet the supplied
``prompt`` / ``input`` before the subagent is spawned. Agent-specific
rules live in the agent's own package (e.g. ``team.scout_dispatch``) and
are registered at module import so any code path that pulls in the
agent's builtins also picks up its validator.
"""

from __future__ import annotations

from typing import Any, Callable

from tools.core.base import ToolExecutionContext, ToolResult

SubagentDispatchValidator = Callable[
    [str | None, dict[str, Any] | None, ToolExecutionContext],
    ToolResult | None,
]

_VALIDATORS: dict[str, SubagentDispatchValidator] = {}


def register_dispatch_validator(agent_name: str, fn: SubagentDispatchValidator) -> None:
    """Register *fn* as the dispatch validator for *agent_name*.

    A later registration for the same name replaces the earlier one.
    """
    _VALIDATORS[agent_name] = fn


def get_dispatch_validator(agent_name: str) -> SubagentDispatchValidator | None:
    """Return the registered validator for *agent_name*, or ``None``."""
    return _VALIDATORS.get(agent_name)
