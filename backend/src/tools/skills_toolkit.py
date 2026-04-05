"""Skills toolkit — lazy-loaded skill access via meta-tools.

Instead of injecting full skill content into the system prompt (which
can consume 10-50K+ tokens), this toolkit provides tools that let the
agent load skill content on demand.  The system prompt only contains
skill name + one-line description (~20 tokens each).

Follows Agno's progressive discovery pattern:
1. Agent sees skill summaries in system prompt
2. Agent calls ``load_skill`` to get full instructions when needed
3. Agent calls ``load_skill_reference`` for supplementary docs
4. Only relevant content consumes context tokens

Usage::

    from tools.skills_toolkit import make_skills_toolkit

    toolkit = make_skills_toolkit(skill_registry, allowed_slugs=["skill-a", "skill-b"])
    tool_registry.register_toolkit(toolkit)
"""

from __future__ import annotations

import json

from tools.base import BaseToolkit, ToolExecutionContext, ToolResult
from tools.decorator import tool
from skills.registry import SkillRegistry


def make_skills_toolkit(
    skill_registry: SkillRegistry,
    allowed_slugs: list[str] | None = None,
) -> BaseToolkit:
    """Create a skills toolkit scoped to the given skill slugs.

    If *allowed_slugs* is None, all registered skills are available.

    The toolkit provides three tools:

    - ``list_skills`` — list available skills with descriptions and reference names
    - ``load_skill`` — load the full instructions (SKILL.md) of a skill
    - ``load_skill_reference`` — load a specific reference document from a skill
    """

    # Pre-resolve allowed skills for fast lookup
    available: dict[str, dict[str, object]] = {}
    slugs = allowed_slugs if allowed_slugs is not None else [s.name for s in skill_registry.list_skills()]
    for slug in slugs:
        skill = skill_registry.get(slug)
        if skill:
            available[skill.name] = {
                "name": skill.name,
                "description": skill.description,
                "references": list(skill.references.keys()),
            }

    @tool(
        name="load_skill",
        description="Load the full instructions for a skill. Call this when a task matches a skill's description.",
        read_only=True,
    )
    async def load_skill(
        skill_name: str,
        *,
        context: ToolExecutionContext,
    ) -> ToolResult:
        """Load full skill instructions by name.

        Args:
            skill_name: Name of the skill to load
        """
        if skill_name not in available:
            return ToolResult(
                output=json.dumps({
                    "error": f"Skill '{skill_name}' not found.",
                    "available": list(available.keys()),
                }),
                is_error=True,
            )

        skill = skill_registry.get(skill_name)
        if skill is None:
            return ToolResult(
                output=f"Skill '{skill_name}' not found in registry.",
                is_error=True,
            )

        # Include reference list so agent knows what's available to load next
        ref_names = list(skill.references.keys())
        if ref_names:
            footer = (
                "\n\n---\n"
                f"This skill has {len(ref_names)} reference document(s) available: "
                + ", ".join(f"`{r}`" for r in ref_names)
                + "\nUse `load_skill_reference` to load any of them."
            )
            return ToolResult(output=skill.content + footer)

        return ToolResult(output=skill.content)

    @tool(
        name="load_skill_reference",
        description="Load a reference document from a skill. References provide supplementary guidance, schemas, rubrics, or examples.",
        read_only=True,
    )
    async def load_skill_reference(
        skill_name: str,
        reference_name: str,
        *,
        context: ToolExecutionContext,
    ) -> ToolResult:
        """Load a specific reference document from a skill.

        Args:
            skill_name: Name of the skill that owns the reference
            reference_name: Name of the reference document to load
        """
        if skill_name not in available:
            return ToolResult(
                output=json.dumps({
                    "error": f"Skill '{skill_name}' not found.",
                    "available": list(available.keys()),
                }),
                is_error=True,
            )

        skill = skill_registry.get(skill_name)
        if skill is None:
            return ToolResult(
                output=f"Skill '{skill_name}' not found in registry.",
                is_error=True,
            )

        content = skill.references.get(reference_name)
        if content is None:
            return ToolResult(
                output=json.dumps({
                    "error": f"Reference '{reference_name}' not found in skill '{skill_name}'.",
                    "available_references": list(skill.references.keys()),
                }),
                is_error=True,
            )

        return ToolResult(output=content)

    # Build skill catalog for instructions
    skill_entries = []
    for info in available.values():
        skill_entries.append(f"  {info['name']}: {info['description']}")

    instructions = (
        "Lazy-loaded skill system. Use `load_skill(skill_name)` when a task "
        "matches a skill's domain. Use `load_skill_reference(skill_name, ref)` "
        "for supplementary docs.\n\n"
        "```yaml\nskills:\n" + "\n".join(skill_entries) + "\n```"
    ) if skill_entries else None

    return BaseToolkit(
        name="skills",
        description="Lazy-loaded skill instructions and reference documents",
        tools=[load_skill, load_skill_reference],
        instructions=instructions,
    )
