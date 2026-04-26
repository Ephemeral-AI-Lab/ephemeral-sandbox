from __future__ import annotations

from pathlib import Path

import pytest

from skills.core.registry import SkillRegistry
from skills.core.types import SkillDefinition
from tools.builtins.skills.factory import make_skills_tools
from tools.core.base import ToolExecutionContextService


@pytest.mark.asyncio
async def test_load_skill_does_not_append_reference_footer() -> None:
    registry = SkillRegistry()
    registry.register(
        SkillDefinition(
            name="demo-skill",
            description="Demo skill.",
            content="# Demo\n\nUse the main workflow.",
            source="test",
            references={"extra": "Supplementary guidance."},
        )
    )
    tools = {tool.name: tool for tool in make_skills_tools(registry)}
    load_skill = tools.get("load_skill")

    assert load_skill is not None
    result = await load_skill.execute(
        load_skill.input_model(skill_name="demo-skill"),
        ToolExecutionContextService(cwd=Path("/tmp")),
    )

    assert result.output == "# Demo\n\nUse the main workflow."
    assert "This skill has" not in result.output
    assert "Use `load_skill_reference` to load any of them." not in result.output


@pytest.mark.asyncio
async def test_load_skill_reference_still_loads_named_references() -> None:
    registry = SkillRegistry()
    registry.register(
        SkillDefinition(
            name="demo-skill",
            description="Demo skill.",
            content="# Demo",
            source="test",
            references={"extra": "Supplementary guidance."},
        )
    )
    tools = {tool.name: tool for tool in make_skills_tools(registry)}
    load_reference = tools.get("load_skill_reference")

    assert load_reference is not None
    result = await load_reference.execute(
        load_reference.input_model(skill_name="demo-skill", reference_name="extra"),
        ToolExecutionContextService(cwd=Path("/tmp")),
    )

    assert result.output == "Supplementary guidance."


@pytest.mark.asyncio
async def test_skill_references_can_load_without_stage_gate() -> None:
    registry = SkillRegistry()
    registry.register(
        SkillDefinition(
            name="demo-skill",
            description="Demo skill.",
            content="# Demo",
            source="test",
            references={"extra": "Supplementary guidance."},
        )
    )
    tools = {tool.name: tool for tool in make_skills_tools(registry)}
    load_skill = tools.get("load_skill")
    load_reference = tools.get("load_skill_reference")
    assert load_skill is not None
    assert load_reference is not None

    context = ToolExecutionContextService(cwd=Path("/tmp"))
    load_result = await load_skill.execute(
        load_skill.input_model(skill_name="demo-skill"),
        context,
    )
    assert load_result.is_error is False

    result = await load_reference.execute(
        load_reference.input_model(
            skill_name="demo-skill",
            reference_name="extra",
        ),
        context,
    )

    assert result.is_error is False
    assert result.output == "Supplementary guidance."
