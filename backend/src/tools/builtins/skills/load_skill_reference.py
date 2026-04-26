"""Factory for the load_skill_reference tool."""

from __future__ import annotations

import json

from pydantic import BaseModel, Field

from skills.core.registry import SkillRegistry
from tools.core.base import BaseTool, TextToolOutput, ToolExecutionContextService, ToolResult
from tools.core.decorator import tool


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


def make_load_skill_reference(
    *,
    skill_registry: SkillRegistry,
    available: dict[str, dict[str, object]],
) -> BaseTool:
    @tool(
        name="load_skill_reference",
        description="Returns one named reference document from a skill.",
        short_description="Load a skill reference.",
        input_model=LoadSkillReferenceInput,
        output_model=TextToolOutput,
    )
    async def load_skill_reference(
        skill_name: str,
        reference_name: str,
        *,
        context: ToolExecutionContextService,
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

        return ToolResult(output=content)

    return load_skill_reference


__all__ = ["make_load_skill_reference"]
