from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from agents.registry import get_definition
from engine.runtime.agent import _build_agent_tool_registry, finalize_tool_registry_and_prompt
from team.definitions import (
    DEVELOPER,
    PARENT_SUMMARIZER,
    ROOT_PLANNER,
    SCOUT,
    TEAM_PLANNER,
    TEAM_REPLANNER,
    VALIDATOR,
    register_all,
)
from prompt.helpers import resolve_terminal_tools
from tools.core.base import ToolRegistry
from tools.ci_toolkit import make_code_intelligence_tools
from tools.daytona_toolkit import make_daytona_tools
from tools.task_center import make_task_center_tools


def setup_module() -> None:
    register_all()


def test_builtin_team_agents_preload_skills_without_lazy_skill_tools() -> None:
    for name in (TEAM_PLANNER, TEAM_REPLANNER, DEVELOPER, VALIDATOR, SCOUT):
        defn = get_definition(name)
        assert defn is not None
        assert defn.include_skills is True
        assert defn.skills, f"{name} should still declare its preloaded playbook"


def test_team_planner_prompt_loads_playbook_before_planning_tools() -> None:
    defn = get_definition(TEAM_PLANNER)
    assert defn is not None
    assert defn.system_prompt is not None
    assert 'load_skill(skill_name="team-planner-playbook")' in defn.system_prompt
    assert "before your first code-intelligence" in defn.system_prompt
    assert "Use that playbook to choose and order references" in defn.system_prompt
    assert "restructured package/directory with multiple plausible owner files" in defn.system_prompt
    assert "do not route sibling ownership from failing test names" in defn.system_prompt
    assert "concrete pytest ids or test files" in defn.system_prompt
    assert "Do not substitute sibling or similarly named test modules" in defn.system_prompt
    assert "Do not convert adjacent, external, or \"likely from X\" hypotheses" in defn.system_prompt
    assert "without live scout evidence that proved the path as a repo owner" in defn.system_prompt
    assert "If a launched scout shows that an inherited exact file is missing" in defn.system_prompt
    assert "only live scout evidence may prove the replacement `scope_paths`" in defn.system_prompt
    assert "Do not ask a single-file scout to inspect additional files or directories" in defn.system_prompt
    assert "launch a separate scout for that path or carry it as uncertainty" in defn.system_prompt


def test_developer_prompt_requires_live_path_proof_for_new_modules() -> None:
    defn = get_definition(DEVELOPER)
    assert defn is not None
    assert defn.system_prompt is not None
    assert "Do not create missing modules, shims, bridges, or re-exports" in defn.system_prompt
    assert "failing test imports, grep hits, or similarly named sibling paths alone" in defn.system_prompt
    assert "replan instead of writing it" in defn.system_prompt
    assert "benchmark import of `dask._compatibility` does not prove" in defn.system_prompt


def test_team_replanner_prompt_loads_playbook_before_planning_tools() -> None:
    defn = get_definition(TEAM_REPLANNER)
    assert defn is not None
    assert defn.system_prompt is not None
    assert "load `team-replanner-playbook` before code-intelligence" in defn.system_prompt
    assert "Use that playbook to choose and order references" in defn.system_prompt


def test_parent_summarizer_prompt_requests_replan_for_unresolved_rollups() -> None:
    defn = get_definition(PARENT_SUMMARIZER)
    assert defn is not None
    assert defn.system_prompt is not None
    assert "`submit_task_success(summary=...)`" in defn.system_prompt
    assert "`request_replan(reason=...)`" in defn.system_prompt
    assert "replan_trigger: unresolved_blocker" in defn.system_prompt
    assert "open risk`, not `delivered`" in defn.system_prompt
    assert "success evidence is invalid when it depends on pytest configuration" in defn.system_prompt
    assert "`--override-ini`" in defn.system_prompt
    assert "whose overridden-evidence child line says `delivered`" in defn.system_prompt
    assert "reported pass uses -p no:warnings" in defn.system_prompt


def test_root_planner_prompt_emphasizes_top_down_decomposition() -> None:
    defn = get_definition(ROOT_PLANNER)
    assert defn is not None
    assert defn.system_prompt is not None
    assert "Use top-down decomposition" in defn.system_prompt
    assert "route broad or unresolved regions to child `team_planner` tasks" in defn.system_prompt
    assert "instead of exhaustively exploring every implementation detail at the root layer" in defn.system_prompt
    assert "prefer child planners even when the first-pass owner labels are clear" in defn.system_prompt
    assert "For clustering jobs, include at least one child `team_planner`" in defn.system_prompt
    assert "not multi-cluster benchmark repair" in defn.system_prompt
    assert "exactly one production owner path in `target_paths`" in defn.system_prompt
    assert "Never bundle two files/directories into one scout" in defn.system_prompt


def test_scout_prompt_loads_playbook_before_exploration_tools() -> None:
    defn = get_definition(SCOUT)
    assert defn is not None
    assert defn.system_prompt is not None
    assert 'load_skill(skill_name="team-scout-playbook")' in defn.system_prompt
    assert 'load_skill_reference(skill_name="team-scout-playbook", reference_name="completion-contract")' in defn.system_prompt
    assert "before your first Task Center or code-intelligence tool call" in defn.system_prompt
    assert "first assistant message that calls tools may contain only `read_file_note" in defn.system_prompt
    assert "stop after exact-file CI evidence" in defn.system_prompt
    assert "Only `target_paths` authorize exploration" in defn.system_prompt
    assert "treat them as hypotheses to report under gaps" in defn.system_prompt
    assert "If an assigned exact file is missing, CI-cold, or disproved by a package/directory boundary" in defn.system_prompt
    assert "Do not search sibling modules, package structure, or helper-symbol names" in defn.system_prompt


def test_builtin_team_agents_use_default_tool_call_limits() -> None:
    for name in (TEAM_PLANNER, TEAM_REPLANNER, DEVELOPER, VALIDATOR, SCOUT):
        defn = get_definition(name)
        assert defn is not None
        assert defn.tool_call_limit == 100


def test_team_agents_share_same_code_intelligence_tool_surface() -> None:
    planner_ci = make_code_intelligence_tools()
    developer_ci = make_code_intelligence_tools()

    assert {tool.name for tool in planner_ci} == {tool.name for tool in developer_ci}


def test_tool_sets_do_not_expose_instruction_blocks() -> None:
    developer_ci = make_code_intelligence_tools()
    sandbox_ops = make_daytona_tools()

    assert all(not hasattr(tool, "instructions") for tool in developer_ci)
    assert all(not hasattr(tool, "instructions") for tool in sandbox_ops)
    assert "daytona_grep" in {tool.name for tool in sandbox_ops}


def test_team_worker_sandbox_tools_include_shell() -> None:
    developer_sandbox = make_daytona_tools()
    validator_sandbox = make_daytona_tools()
    developer_names = {tool.name for tool in developer_sandbox}
    validator_names = {tool.name for tool in validator_sandbox}

    assert "daytona_shell" in developer_names
    assert "daytona_shell" in validator_names
    assert "daytona_edit_file" in developer_names
    # daytona_bash has been removed — all agents use daytona_shell
    assert "daytona_bash" not in developer_names
    assert "daytona_bash" not in validator_names


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
        terminal_tools=resolve_terminal_tools(defn),
    )
    return {tool.name for tool in registry.list_tools()}


def _final_prompt(name: str, tmp_path: Path) -> str:
    defn = get_definition(name)
    assert defn is not None
    registry = _build_agent_tool_registry(
        SimpleNamespace(cwd=str(tmp_path)),
        defn,
        "sb-test",
        defn.name,
    )
    prompt, _ = finalize_tool_registry_and_prompt(
        registry,
        defn.system_prompt or "",
        can_spawn_subagents=defn.can_spawn_subagents,
        terminal_tools=resolve_terminal_tools(defn),
    )
    return prompt


def test_planner_and_replanner_do_not_expose_sandbox_tools(tmp_path: Path) -> None:
    for name in (TEAM_PLANNER, TEAM_REPLANNER):
        tool_names = _final_tool_names(name, tmp_path)
        for tool_name in (
            "daytona_grep",
            "daytona_glob",
            "daytona_read_file",
            "daytona_write_file",
            "daytona_edit_file",
            "daytona_shell",
        ):
            assert tool_name not in tool_names


def test_scout_tool_surface_matches_note_handoff_contract(tmp_path: Path) -> None:
    tool_names = _final_tool_names(SCOUT, tmp_path)

    assert "submit_file_notes" in tool_names
    for name in (
        "daytona_grep",
        "daytona_glob",
        "daytona_read_file",
        "daytona_write_file",
        "daytona_edit_file",
        "daytona_shell",
        "submit_task_success",
        "submit_plan",
        "submit_replan",
        "read_task_details",
        "read_task_graph",
    ):
        assert name not in tool_names


def test_parent_summarizer_tool_surface_is_read_only_except_terminal_summary(
    tmp_path: Path,
) -> None:
    tool_names = _final_tool_names(PARENT_SUMMARIZER, tmp_path)

    assert {"read_task_details", "read_task_graph", "submit_task_success"} <= tool_names
    for name in (
        "submit_file_notes",
        "submit_plan",
        "submit_replan",
    ):
        assert name not in tool_names


def test_root_planner_tool_surface_blocks_direct_context_and_diagnostics(tmp_path: Path) -> None:
    tool_names = _final_tool_names(ROOT_PLANNER, tmp_path)

    for name in (
        "read_task_graph",
        "ci_status",
        "ci_diagnostics",
        "read_task_details",
    ):
        assert name not in tool_names


def test_root_planner_prompt_omits_awareness_sections(tmp_path: Path) -> None:
    prompt = _final_prompt(ROOT_PLANNER, tmp_path)

    assert "<Available Skills>" not in prompt
    assert "<Background Tasks>" not in prompt
    assert "<Termination Condition>" in prompt
    assert "- `submit_plan`" in prompt


def test_task_center_tools_survive_restriction() -> None:
    registry = ToolRegistry()
    registry.register_many(make_task_center_tools())
    registry.restrict_to_tools(["read_file_note", "read_task_details"])

    assert registry.get("read_file_note") is not None
    assert registry.get("read_task_details") is not None
    assert registry.get("read_task_graph") is None
