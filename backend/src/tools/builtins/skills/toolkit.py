"""Skill loading tools.

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

import json

from pydantic import BaseModel, Field

from config.defaults import SKILL_REFERENCE_TRACE_LIMIT
from tools.core.base import BaseTool, TextToolOutput, ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from skills.core.registry import SkillRegistry

_LOADED_SKILL_REFERENCES_KEY = "_loaded_skill_references_by_skill_this_turn"
_NON_REFERENCE_TOOL_CALLS_SINCE_SKILL_LOAD_KEY = (
    "_non_reference_tool_calls_since_skill_load"
)
_STAGED_PLANNER_REFERENCES = frozenset(
    {
        ("team-root-planner-playbook", "synthesize-and-submit"),
        ("team-planner-playbook", "submit-child-plan"),
    }
)


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

    def _reject_premature_staged_reference(
        context: ToolExecutionContext,
        *,
        skill_name: str,
        reference_name: str,
    ) -> ToolResult | None:
        if (skill_name, reference_name) not in _STAGED_PLANNER_REFERENCES:
            return None
        raw = context.metadata.get(_NON_REFERENCE_TOOL_CALLS_SINCE_SKILL_LOAD_KEY, {})
        if not isinstance(raw, dict) or raw.get(skill_name) != 0:
            return None
        return ToolResult(
            output=json.dumps(
                {
                    "error": "Premature staged planner reference load.",
                    "skill_name": skill_name,
                    "reference_name": reference_name,
                    "required_action": (
                        "Complete the playbook's Analyze/Scout work first. "
                        "Build the owner ledger, launch required scouts or "
                        "carry explicit uncertainty, join the scout wave, and "
                        "read available notes before retrying this reference."
                    ),
                }
            ),
            is_error=True,
        )

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

        return ToolResult(output=skill.content)

    @tool(
        name="load_skill_reference",
        description=(
            "Load a named reference document from a skill. Use only exact "
            "reference names listed in the skill catalog or skill instructions; "
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

        premature = _reject_premature_staged_reference(
            context,
            skill_name=skill_name,
            reference_name=reference_name,
        )
        if premature is not None:
            return premature

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

    return [load_skill, load_skill_reference]
