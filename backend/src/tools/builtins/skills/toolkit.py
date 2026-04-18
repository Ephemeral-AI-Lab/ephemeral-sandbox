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

    from tools.builtins.skills import make_skills_toolkit

    toolkit = make_skills_toolkit(skill_registry, allowed_slugs=["skill-a", "skill-b"])
    tool_registry.register_toolkit(toolkit)
"""

from __future__ import annotations

import json

from pydantic import BaseModel, Field

from config.defaults import SKILL_REFERENCE_TRACE_LIMIT
from tools.core.base import BaseToolkit, TextToolOutput, ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from skills.core.registry import SkillRegistry

_LOADED_SKILL_REFERENCES_KEY = "_loaded_skill_references_by_skill_this_turn"


class LoadSkillInput(BaseModel):
    skill_name: str = Field(
        ...,
        description="Name of the skill to load.",
    )


class LoadSkillReferenceInput(BaseModel):
    skill_name: str = Field(
        ...,
        description="Name of the skill that owns the reference.",
    )
    reference_name: str = Field(
        ...,
        description=(
            "Exact reference document name to load. Do not use 'default'; call "
            "load_skill(skill_name) for the main skill instructions."
        ),
    )


def make_skills_toolkit(
    skill_registry: SkillRegistry,
    allowed_slugs: list[str] | None = None,
) -> BaseToolkit:
    """Create a skills toolkit scoped to the given skill slugs.

    If *allowed_slugs* is None, all registered skills are available.

    The toolkit provides two tools:

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

    def _loaded_skill_references(
        context: ToolExecutionContext,
        *,
        skill_name: str,
    ) -> list[str]:
        raw = context.metadata.get(_LOADED_SKILL_REFERENCES_KEY, {})
        if not isinstance(raw, dict):
            return []
        refs = raw.get(skill_name, [])
        if not isinstance(refs, list):
            return []
        out: list[str] = []
        for item in refs:
            if isinstance(item, str):
                stripped = item.strip()
                if stripped:
                    out.append(stripped)
        return out

    def _record_loaded_skill_reference(
        context: ToolExecutionContext,
        *,
        skill_name: str,
        reference_name: str,
    ) -> None:
        raw = context.metadata.get(_LOADED_SKILL_REFERENCES_KEY, {})
        loaded = raw.copy() if isinstance(raw, dict) else {}
        refs = _loaded_skill_references(context, skill_name=skill_name)
        refs.append(reference_name)
        if len(refs) > SKILL_REFERENCE_TRACE_LIMIT:
            refs = refs[-SKILL_REFERENCE_TRACE_LIMIT:]
        loaded[skill_name] = refs
        context.metadata[_LOADED_SKILL_REFERENCES_KEY] = loaded

    @tool(
        name="load_skill",
        description="Load the full instructions for a skill. Call this when a task matches a skill's description.",
        short_description="Load a skill's instructions.",
        input_model=LoadSkillInput,
        output_model=TextToolOutput,
    )
    async def load_skill(
        skill_name: str,
        *,
        context: ToolExecutionContext,
    ) -> ToolResult:
        """Load full skill instructions by name."""
        if skill_name not in available:
            return ToolResult(
                output=json.dumps(
                    {
                        "error": f"Skill '{skill_name}' not found.",
                        "available": list(available.keys()),
                    }
                ),
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
        description=(
            "Load a named reference document from a skill. Use only exact "
            "reference names listed in the skill catalog or in load_skill output; "
            "there is no default reference."
        ),
        short_description="Load a skill reference.",
        input_model=LoadSkillReferenceInput,
        output_model=TextToolOutput,
    )
    async def load_skill_reference(
        skill_name: str,
        reference_name: str,
        *,
        context: ToolExecutionContext,
    ) -> ToolResult:
        """Load a specific reference document from a skill."""
        if skill_name not in available:
            return ToolResult(
                output=json.dumps(
                    {
                        "error": f"Skill '{skill_name}' not found.",
                        "available": list(available.keys()),
                    }
                ),
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
                output=json.dumps(
                    {
                        "error": f"Reference '{reference_name}' not found in skill '{skill_name}'.",
                        "available_references": list(skill.references.keys()),
                    }
                ),
                is_error=True,
            )

        _record_loaded_skill_reference(
            context,
            skill_name=skill_name,
            reference_name=reference_name,
        )
        return ToolResult(output=content)

    # Build skill catalog for instructions
    skill_entries = []
    for info in available.values():
        refs = info.get("references", [])
        ref_note = f" ({len(refs)} references)" if refs else ""
        skill_entries.append(f'- `load_skill("{info["name"]}")` — {info["description"]}{ref_note}')

    instructions = (
        (
            "Lazy-loaded skill system. Use `load_skill(skill_name)` when a task "
            "matches a skill's domain. Use `load_skill_reference(skill_name, ref)` "
            "for supplementary docs.\n\n"
            "**Available skills:**\n" + "\n".join(skill_entries)
        )
        if skill_entries
        else None
    )

    toolkit = BaseToolkit(
        name="skills",
        description="Lazy-loaded skill instructions and reference documents",
        tools=[load_skill, load_skill_reference],
        instructions=instructions,
    )
    if allowed_slugs is not None:
        toolkit.available_skills = [
            {
                "name": str(info["name"]),
                "description": str(info["description"] or ""),
                "references": [str(ref) for ref in info.get("references", [])],
            }
            for info in sorted(available.values(), key=lambda item: str(item["name"]))
        ]
    return toolkit
