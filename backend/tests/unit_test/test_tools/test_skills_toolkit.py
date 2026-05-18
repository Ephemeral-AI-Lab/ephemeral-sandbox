from __future__ import annotations

from pathlib import Path

import pytest

from agents import AgentDefinition, AgentKind, register_definition, unregister_definition
from skills.core.registry import SkillRegistry
from skills.core.types import SkillDefinition
from tools._framework.factory import ToolFactoryContext
from tools.skills._factory import (
    make_load_skill_reference_for_skill,
    make_load_skill_reference_from_context,
)
from tools._framework.core.base import ToolExecutionContextService


def _registry_with_demo_skill() -> SkillRegistry:
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
    return registry


@pytest.mark.asyncio
async def test_load_skill_reference_resolves_named_reference() -> None:
    registry = _registry_with_demo_skill()
    tool = make_load_skill_reference_for_skill(
        skill_slug="demo-skill", skill_registry=registry
    )

    result = await tool.execute(
        tool.input_model(skill_name="demo-skill", reference_name="extra"),
        ToolExecutionContextService(cwd=Path("/tmp")),
    )

    assert result.is_error is False
    assert result.output == "Supplementary guidance."


@pytest.mark.asyncio
async def test_load_skill_reference_rejects_unscoped_skill() -> None:
    """A planner can only resolve references inside its own skill folder."""
    registry = _registry_with_demo_skill()
    registry.register(
        SkillDefinition(
            name="other-skill",
            description="Other skill.",
            content="# Other",
            source="test",
            references={"sibling": "Other guidance."},
        )
    )
    tool = make_load_skill_reference_for_skill(
        skill_slug="demo-skill", skill_registry=registry
    )

    result = await tool.execute(
        tool.input_model(skill_name="other-skill", reference_name="sibling"),
        ToolExecutionContextService(cwd=Path("/tmp")),
    )

    assert result.is_error is True
    assert "not found" in result.output


@pytest.mark.asyncio
async def test_load_skill_reference_with_no_skill_slug_rejects_everything() -> None:
    """Agents that declare the tool without a skill see a defensive no-op."""
    registry = _registry_with_demo_skill()
    tool = make_load_skill_reference_for_skill(
        skill_slug=None, skill_registry=registry
    )

    result = await tool.execute(
        tool.input_model(skill_name="demo-skill", reference_name="extra"),
        ToolExecutionContextService(cwd=Path("/tmp")),
    )

    assert result.is_error is True


@pytest.mark.asyncio
async def test_load_skill_reference_from_context_uses_agent_skill_folder(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    registry = _registry_with_demo_skill()
    skill_file = tmp_path / "demo-skill" / "SKILL.md"
    skill_file.parent.mkdir()
    skill_file.write_text("# Demo", encoding="utf-8")
    definition = AgentDefinition(
        name="planner_ctx",
        description="Planner with a skill.",
        agent_kind=AgentKind.PLANNER,
        skill=skill_file,
    )
    register_definition(definition)
    monkeypatch.setattr("tools.skills._factory._registry", lambda: registry)

    try:
        tool = make_load_skill_reference_from_context(
            ToolFactoryContext(metadata={"agent_name": "planner_ctx"})
        )
        result = await tool.execute(
            tool.input_model(skill_name="demo-skill", reference_name="extra"),
            ToolExecutionContextService(cwd=Path("/tmp")),
        )
    finally:
        unregister_definition("planner_ctx")

    assert result.is_error is False
    assert result.output == "Supplementary guidance."
