from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from skills.core.registry import SkillRegistry
from skills.core.types import SkillDefinition
from team.runtime.registry import register as register_team_run
from team.runtime.registry import unregister as unregister_team_run
from tools.builtins.skills import make_skills_toolkit
from tools.core.base import ToolExecutionContext
from tools.core.runtime import ExecutionMetadata


def _registry() -> SkillRegistry:
    registry = SkillRegistry()
    registry.register(
        SkillDefinition(
            name="team-planner-playbook",
            description="planner skill",
            content="planner",
            source="test",
            references={
                "plan-json-contract": "plan-json",
                "task-planning-decomposition": "decomposition",
            },
        )
    )
    return registry


def _benchmark_root_context() -> ToolExecutionContext:
    metadata = ExecutionMetadata()
    metadata["agent_name"] = "team_planner"
    metadata["team_run_id"] = "team-run-1"
    metadata["work_item_id"] = "root-1"
    return ToolExecutionContext(cwd=Path.cwd(), metadata=metadata)


@pytest.mark.asyncio
async def test_skills_toolkit_blocks_final_plan_references_before_first_scout_wave():
    team_run = SimpleNamespace(
        id="team-run-1",
        root_work_item_id="root-1",
        dispatcher=SimpleNamespace(
            graph={
                "root-1": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_cli.py::test_versions"]}
                )
            }
        ),
    )
    register_team_run(team_run)
    try:
        toolkit = make_skills_toolkit(_registry(), allowed_slugs=["team-planner-playbook"])
        tool = toolkit.get("load_skill_reference")
        assert tool is not None

        result = await tool.execute(
            tool.input_model(
                skill_name="team-planner-playbook",
                reference_name="plan-json-contract",
            ),
            _benchmark_root_context(),
        )

        assert result.is_error
        assert "run_subagent(agent_name=\"scout\"" in result.output
        assert "before the first scout wave" in result.output
    finally:
        unregister_team_run("team-run-1")


@pytest.mark.asyncio
async def test_skills_toolkit_allows_final_plan_reference_after_scout_wave():
    team_run = SimpleNamespace(
        id="team-run-1",
        root_work_item_id="root-1",
        dispatcher=SimpleNamespace(
            graph={
                "root-1": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_cli.py::test_versions"]}
                )
            }
        ),
    )
    register_team_run(team_run)
    try:
        toolkit = make_skills_toolkit(_registry(), allowed_slugs=["team-planner-playbook"])
        tool = toolkit.get("load_skill_reference")
        assert tool is not None
        ctx = _benchmark_root_context()
        ctx.metadata["_scout_target_paths_this_turn"] = ["pkg/cli.py"]

        result = await tool.execute(
            tool.input_model(
                skill_name="team-planner-playbook",
                reference_name="plan-json-contract",
            ),
            ctx,
        )

        assert not result.is_error
        assert result.output == "plan-json"
    finally:
        unregister_team_run("team-run-1")
