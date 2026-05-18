"""Skill-reference tool factory.

Round 3 ships a single skill-related tool, ``load_skill_reference``. The
skill body itself lands as row 4 at agent launch (see
``task_center/context_engine/core.py:build_skill_message``); references
under the loaded skill's ``references/`` folder are reachable on demand
via this tool.

Two factory shapes are exposed:

* :func:`make_load_skill_reference_from_context` — registered as a
  global tool factory in :mod:`tools._framework.factory`. Resolves the
  spawning agent's name at create time, looks up its
  ``AgentDefinition.skill``, and scopes ``allowed_slugs`` to the skill
  folder name so a planner can only read its own skill's references.

* :func:`make_load_skill_reference_for_skill` — used by tests to build
  the tool with an explicit slug allowlist (no agent registry round
  trip).
"""

from __future__ import annotations

from functools import lru_cache

from skills.core.registry import SkillRegistry
from tools._framework.core.base import BaseTool
from tools._framework.factory import ToolFactoryContext
from tools.skills.load_skill_reference import make_load_skill_reference


@lru_cache(maxsize=1)
def _registry() -> SkillRegistry:
    """Process-global skill registry, populated from bundled config."""
    from skills.core.loader import load_skill_registry

    return load_skill_registry()


def make_load_skill_reference_for_skill(
    *,
    skill_slug: str | None,
    skill_registry: SkillRegistry | None = None,
) -> BaseTool:
    """Build a ``load_skill_reference`` tool scoped to one skill slug.

    Passing ``None`` for ``skill_slug`` produces a tool that surfaces
    "no skill loaded" errors on every call — useful as a defensive
    no-op when an agent declares the tool but no skill resolved.
    """
    registry = skill_registry if skill_registry is not None else _registry()
    allowed_slugs: list[str] = [skill_slug] if skill_slug else []
    available: dict[str, dict[str, object]] = {}
    for slug in allowed_slugs:
        skill = registry.get(slug)
        if skill is None:
            continue
        available[skill.name] = {
            "name": skill.name,
            "description": skill.description,
            "references": list(skill.references.keys()),
        }
    return make_load_skill_reference(
        skill_registry=registry, available=available
    )


def make_load_skill_reference_from_context(
    ctx: ToolFactoryContext,
) -> BaseTool:
    """Build ``load_skill_reference`` for the spawning agent.

    Reads ``ctx.metadata["agent_name"]``, looks up the agent definition,
    and scopes the tool to the agent's own ``skill`` folder name. An
    agent that lists ``load_skill_reference`` in ``allowed_tools`` but
    has no ``skill:`` declared receives a no-op tool whose every call
    reports "skill not found" — by design; the tool surface should
    follow the skill declaration.
    """
    from agents import get_definition

    agent_name = str(ctx.metadata.get("agent_name") or "")
    skill_slug: str | None = None
    if agent_name:
        defn = get_definition(agent_name)
        if defn is not None and defn.skill is not None:
            skill_slug = defn.skill.parent.name
    return make_load_skill_reference_for_skill(skill_slug=skill_slug)


__all__ = [
    "make_load_skill_reference_for_skill",
    "make_load_skill_reference_from_context",
]
