from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from agents.registry import get_definition
from engine.runtime.agent import _build_agent_tool_registry, finalize_tool_registry_and_prompt
from team.builtins import (
    DEVELOPER,
    SCOUT,
    TEAM_PLANNER,
    TEAM_REPLANNER,
    VALIDATOR,
    register_all,
)
from tools.core.base import ToolRegistry
from tools.core.factory import ToolkitContext, create_toolkit


def setup_module() -> None:
    register_all()


def test_builtin_team_agents_preload_skills_without_lazy_skill_toolkit() -> None:
    for name in (TEAM_PLANNER, TEAM_REPLANNER, DEVELOPER, VALIDATOR, SCOUT):
        defn = get_definition(name)
        assert defn is not None
        assert defn.include_skills is True
        assert defn.skills, f"{name} should still declare its preloaded playbook"


def test_team_planner_prompt_loads_playbook_before_planning_tools() -> None:
    defn = get_definition(TEAM_PLANNER)
    assert defn is not None
    assert defn.system_prompt is not None
    assert "load `team-planner-playbook` before code-intelligence" in defn.system_prompt
    assert "Use that playbook to choose and order references" in defn.system_prompt


def test_builtin_team_agents_use_default_tool_call_limits() -> None:
    for name in (TEAM_PLANNER, TEAM_REPLANNER, DEVELOPER, VALIDATOR, SCOUT):
        defn = get_definition(name)
        assert defn is not None
        assert defn.tool_call_limit == 100


def test_team_agents_share_same_code_intelligence_toolkit_surface() -> None:
    planner_ci = create_toolkit(
        "code_intelligence",
        ToolkitContext(metadata={"agent_name": TEAM_PLANNER}),
    )
    developer_ci = create_toolkit(
        "code_intelligence",
        ToolkitContext(metadata={"agent_name": DEVELOPER}),
    )

    assert set(planner_ci.tool_names()) == set(developer_ci.tool_names())


def test_toolkit_instructions_surface_scope_and_search_tools() -> None:
    developer_ci = create_toolkit(
        "code_intelligence",
        ToolkitContext(metadata={"agent_name": DEVELOPER}),
    )
    sandbox_ops = create_toolkit(
        "sandbox_operations",
        ToolkitContext(metadata={"sandbox_id": "sb-test"}),
    )

    assert developer_ci.instructions is not None

    assert sandbox_ops.instructions is not None
    assert "daytona_grep" in sandbox_ops.instructions


def test_team_worker_sandbox_toolkit_includes_codeact() -> None:
    developer_sandbox = create_toolkit(
        "sandbox_operations",
        ToolkitContext(metadata={"agent_name": DEVELOPER, "sandbox_id": "sb-dev"}),
    )
    validator_sandbox = create_toolkit(
        "sandbox_operations",
        ToolkitContext(metadata={"agent_name": VALIDATOR, "sandbox_id": "sb-val"}),
    )

    assert "daytona_codeact" in developer_sandbox.tool_names()
    assert "daytona_codeact" in validator_sandbox.tool_names()
    assert "daytona_edit_file" in developer_sandbox.tool_names()
    assert "daytona_rename_symbol" in developer_sandbox.tool_names()
    # daytona_bash has been removed — all agents use daytona_codeact
    assert "daytona_bash" not in developer_sandbox.tool_names()
    assert "daytona_bash" not in validator_sandbox.tool_names()


def _final_tool_names(name: str, tmp_path: Path) -> set[str]:
    defn = get_definition(name)
    assert defn is not None
    registry = _build_agent_tool_registry(
        SimpleNamespace(cwd=str(tmp_path)),
        defn,
        "sb-test",
        defn.name,
    )
    finalize_tool_registry_and_prompt(
        registry,
        defn.system_prompt or "",
        can_spawn_subagents=defn.can_spawn_subagents,
        role=defn.role,
        blocked_tools=defn.blocked_tools,
        terminal_tools=set(),
    )
    return {tool.name for tool in registry.list_tools()}


def test_planner_and_replanner_do_not_expose_sandbox_tools(tmp_path: Path) -> None:
    for name in (TEAM_PLANNER, TEAM_REPLANNER):
        tool_names = _final_tool_names(name, tmp_path)
        for tool_name in (
            "daytona_grep",
            "daytona_glob",
            "daytona_read_file",
            "daytona_write_file",
            "daytona_edit_file",
            "daytona_rename_symbol",
            "daytona_codeact",
        ):
            assert tool_name not in tool_names


def test_scout_tool_surface_matches_note_handoff_contract(tmp_path: Path) -> None:
    tool_names = _final_tool_names(SCOUT, tmp_path)

    assert "submit_task_note" in tool_names
    assert "task_center_changed_since" not in tool_names
    for name in (
        "daytona_grep",
        "daytona_glob",
        "daytona_read_file",
        "daytona_write_file",
        "daytona_edit_file",
        "daytona_rename_symbol",
        "daytona_codeact",
        "submit_task_summary",
        "submit_plan",
        "submit_replan",
    ):
        assert name not in tool_names


def test_task_center_toolkit_survives_restriction() -> None:
    task_center_toolkit = create_toolkit(
        "task_center",
        ToolkitContext(metadata={"agent_name": TEAM_PLANNER}),
    )
    registry = ToolRegistry()
    registry.register_toolkit(task_center_toolkit)
    registry.restrict_to_toolkits(["task_center"])

    assert registry.get_toolkit("task_center") is not None
    assert registry.get("read_task_note") is not None
    # post_note moved to submission toolkit (terminal tools only)
    assert registry.get("task_center_changed_since") is not None
