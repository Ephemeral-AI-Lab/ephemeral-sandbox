"""Factory for the load_skill tool."""

from __future__ import annotations

import json

from pydantic import BaseModel, Field

from skills.core.registry import SkillRegistry
from tools.core.base import BaseTool, TextToolOutput, ToolExecutionContextService, ToolResult
from tools.core.decorator import tool


class LoadSkillInput(BaseModel):
    skill_name: str = Field(
        ...,
        description="Name of the skill to load.",
    )


def make_load_skill(
    *,
    skill_registry: SkillRegistry,
    available: dict[str, dict[str, object]],
) -> BaseTool:
    @tool(
        name="load_skill",
        description="Returns the full instruction document for a named skill.",
        short_description="Load a skill's instructions.",
        input_model=LoadSkillInput,
        output_model=TextToolOutput,
    )
    async def load_skill(
        skill_name: str,
        *,
        context: ToolExecutionContextService,
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

    return load_skill


__all__ = ["make_load_skill"]
