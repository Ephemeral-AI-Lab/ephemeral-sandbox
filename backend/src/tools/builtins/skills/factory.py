"""Skill loading tool factory.

Instead of injecting full skill content into the system prompt (which
can consume 10-50K+ tokens), these tools let the
agent load skill content on demand.  The system prompt only contains
skill name + one-line description (~20 tokens each).

Follows Agno's progressive discovery pattern:
1. Agent sees skill summaries in system prompt
2. Agent calls ``load_skill`` to get full instructions when needed
3. Agent calls ``load_skill_reference`` for supplementary docs
4. Only relevant content consumes context tokens

Usage::

    from tools.builtins.skills import make_skills_tools

    tool_registry.register_many(make_skills_tools(skill_registry, allowed_slugs=["skill-a", "skill-b"]))
"""

from __future__ import annotations

from skills.core.registry import SkillRegistry
from tools.builtins.skills.load_skill_reference import (
    make_load_skill_reference,
)
from tools.builtins.skills.load_skill import make_load_skill
from tools.core.base import BaseTool


def make_skills_tools(
    skill_registry: SkillRegistry,
    allowed_slugs: list[str] | None = None,
) -> list[BaseTool]:
    """Create skill loading tools scoped to the given skill slugs.

    If *allowed_slugs* is None, all registered skills are available.

    This provides two tools:

    - ``load_skill`` — load the full instructions (SKILL.md) of a skill
    - ``load_skill_reference`` — load a specific reference document from a skill
    """

    # Pre-resolve allowed skills for fast lookup
    available: dict[str, dict[str, object]] = {}
    slugs = (
        allowed_slugs
        if allowed_slugs is not None
        else [s.name for s in skill_registry.list_skills()]
    )
    for slug in slugs:
        skill = skill_registry.get(slug)
        if skill:
            available[skill.name] = {
                "name": skill.name,
                "description": skill.description,
                "references": list(skill.references.keys()),
            }

    return [
        make_load_skill(skill_registry=skill_registry, available=available),
        make_load_skill_reference(
            skill_registry=skill_registry,
            available=available,
        ),
    ]
