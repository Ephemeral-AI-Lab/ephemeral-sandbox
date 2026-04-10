from __future__ import annotations

from agents.registry import get_definition
from team.builtins import (
    DECISION_SUBMIT_REPLAN,
    DECISION_SUBMIT_RETRY,
    DEVELOPER,
    SCOUT,
    TEAM_PLANNER,
    VALIDATOR,
    register_all,
)
from tools.core.factory import ToolkitContext, create_toolkit


def setup_module() -> None:
    register_all()


def test_builtin_team_agents_preload_skills_without_lazy_skill_toolkit() -> None:
    for name in (TEAM_PLANNER, DEVELOPER, VALIDATOR, SCOUT):
        defn = get_definition(name)
        assert defn is not None
        assert defn.include_skills is True
        assert defn.skills, f"{name} should still declare its preloaded playbook"


def test_builtin_team_agents_use_default_tool_call_limits() -> None:
    for name in (TEAM_PLANNER, DEVELOPER, VALIDATOR, SCOUT):
        defn = get_definition(name)
        assert defn is not None
        assert defn.tool_call_limit == 100


def test_decision_posthook_agents_preload_decision_playbook_without_lazy_skill_toolkit() -> None:
    for name in (DECISION_SUBMIT_RETRY, DECISION_SUBMIT_REPLAN):
        defn = get_definition(name)
        assert defn is not None
        assert defn.include_skills is True
        assert defn.skills == ["team-posthook-decision-playbook"]


def test_team_planner_code_intelligence_toolkit_omits_ci_read_file() -> None:
    planner_ci = create_toolkit(
        "code_intelligence",
        ToolkitContext(metadata={"agent_name": TEAM_PLANNER}),
    )
    developer_ci = create_toolkit(
        "code_intelligence",
        ToolkitContext(metadata={"agent_name": DEVELOPER}),
    )

    assert "ci_read_file" not in planner_ci.tool_names()
    assert "ci_read_file" in developer_ci.tool_names()


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
    assert "ci_scope_status" in developer_ci.instructions

    assert sandbox_ops.instructions is not None
    assert "daytona_grep" in sandbox_ops.instructions


def test_team_worker_sandbox_toolkit_omits_codeact() -> None:
    developer_sandbox = create_toolkit(
        "sandbox_operations",
        ToolkitContext(metadata={"agent_name": DEVELOPER, "sandbox_id": "sb-dev"}),
    )
    validator_sandbox = create_toolkit(
        "sandbox_operations",
        ToolkitContext(metadata={"agent_name": VALIDATOR, "sandbox_id": "sb-val"}),
    )

    assert "daytona_codeact" not in developer_sandbox.tool_names()
    assert "daytona_codeact" not in validator_sandbox.tool_names()
    assert "daytona_edit_file" in developer_sandbox.tool_names()
    assert "daytona_bash" in validator_sandbox.tool_names()
