"""Type-level and role-level prompt templates for agent definitions.

The structured prompt system composes three layers:
  1. **Type section** — behavioural constraints derived from ``agent_type``
  2. **Role section** — domain boundaries derived from ``role``
  3. **Agent body** — task-specific instructions from the ``.md`` file

These sections are prepended to the agent's ``.md`` body by
``_build_agent_system_prompt`` in ``engine.runtime.agent``.
"""

from __future__ import annotations

import logging
from types import MappingProxyType

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Type-level templates (keyed by AgentDefinition.agent_type)
# ---------------------------------------------------------------------------
# Use ``{{name}}`` for the agent's registered name.

TYPE_TEMPLATES: MappingProxyType[str, str] = MappingProxyType({
    "agent": (
        "# Identity\n"
        "You are {{name}}. You are a team agent dispatched via the work-item DAG.\n"
        "\n"
        "# Type Constraints\n"
        "- Use the tools available to you. Adhere to preloaded skills as your "
        "authoritative workflow.\n"
        "- You are dispatched by the team executor. Complete your assigned "
        "WorkItem and produce structured output for downstream consumers."
    ),
    "subagent": (
        "# Identity\n"
        "You are {{name}}. You are a focused subagent worker that executes one "
        "bounded task and returns structured output.\n"
        "\n"
        "# Type Constraints\n"
        "- Must not spawn subagents or launch background tasks.\n"
        "- Must not hand off work. Complete the assigned scope or fail explicitly.\n"
        "- You run on a dedicated API client concurrently with sibling workers."
    ),
})

# ---------------------------------------------------------------------------
# Role-level templates (keyed by AgentDefinition.role)
# ---------------------------------------------------------------------------

# Note: the builtin ``validator`` agent uses ``role: reviewer`` because the
# role describes the *function* (code review / verification), not the agent name.
ROLE_TEMPLATES: MappingProxyType[str, str] = MappingProxyType({
    "planner": (
        "# Role Boundary\n"
        "- Must produce a valid plan payload and stop. Do not execute code, "
        "run tests, or write files.\n"
        "- Must not use scout as a proxy for developer or validator work.\n"
        "- Must not add speculative items or items outside the scope of the "
        "incoming request.\n"
        "- Must never submit scout directly as a WorkItem target."
    ),
    "developer": (
        "# Role Boundary\n"
        "- Must stay within the exact scope of the WorkItem payload. Do not "
        "refactor unrelated code or add speculative features.\n"
        "- Must use the literal sandbox tool names exposed at runtime; do not "
        "assume generic aliases.\n"
        "- Must not mutate repo files through shell when direct edit or write "
        "tools are the better fit.\n"
        "- Must not spawn subagents or hand off work."
    ),
    "reviewer": (
        "# Role Boundary\n"
        "- Must not modify repository or production files as part of validation.\n"
        "- Operate in read or execute mode only, except for explicit scratch "
        "artifacts requested by the payload.\n"
        "- Must run scoped verification commands and capture evidence faithfully.\n"
        "- Must return a truthful PASS or FAIL verdict with command, exit-code, "
        "and failure evidence."
    ),
    "explorer": (
        "# Role Boundary\n"
        "- Must stay read-only within the assigned target paths.\n"
        "- Must not inspect .git, reflogs, commit history, or unrelated "
        "workspace areas.\n"
        "- Stop once a downstream worker could act without reopening the same scope."
    ),
    "replanner": (
        "# Role Boundary\n"
        "- Must read the failure context, completed sibling artifacts, and the "
        "original payload before drafting corrections.\n"
        "- Must use only read-only live confirmation if additional context is "
        "needed. You are not an executor.\n"
        "- Do not retry the identical approach that already failed. Diagnose "
        "before prescribing."
    ),
})


def _render(template: str, variables: dict[str, str]) -> str:
    """Minimal ``{{var}}`` substitution."""
    for key, value in variables.items():
        template = template.replace("{{" + key + "}}", value)
    return template


def build_type_section(agent_type: str, name: str) -> str:
    """Return the rendered type-level template for *agent_type*, or ``""``."""
    template = TYPE_TEMPLATES.get(agent_type, "")
    if not template:
        logger.warning("No type template for agent_type=%r (agent=%r)", agent_type, name)
        return ""
    return _render(template, {"name": name})


def build_role_section(role: str | None) -> str:
    """Return the role-level template for *role*, or ``""``."""
    if not role:
        return ""
    template = ROLE_TEMPLATES.get(role, "")
    if not template:
        logger.warning("No role template for role=%r", role)
        return ""
    return template
