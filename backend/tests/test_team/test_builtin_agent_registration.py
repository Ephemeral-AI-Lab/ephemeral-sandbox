from __future__ import annotations

from agents.registry import get_definition
from team.builtins import (
    ATLAS_BUILDER,
    ATLAS_REFRESHER,
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
    for name in (TEAM_PLANNER, DEVELOPER, VALIDATOR, SCOUT, ATLAS_BUILDER, ATLAS_REFRESHER):
        defn = get_definition(name)
        assert defn is not None
        assert defn.include_skills is False
        assert defn.skills, f"{name} should still declare its preloaded playbook"


def test_builtin_team_agents_use_default_tool_call_limits() -> None:
    for name in (TEAM_PLANNER, DEVELOPER, VALIDATOR, SCOUT, ATLAS_BUILDER, ATLAS_REFRESHER):
        defn = get_definition(name)
        assert defn is not None
        assert defn.tool_call_limit == 100


def test_decision_posthook_agents_do_not_declare_skills() -> None:
    for name in (DECISION_SUBMIT_RETRY, DECISION_SUBMIT_REPLAN):
        defn = get_definition(name)
        assert defn is not None
        assert defn.include_skills is False
        assert defn.skills == []


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
