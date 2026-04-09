from __future__ import annotations

from agents.registry import get_definition
from team.builtins import (
    ATLAS_BUILDER,
    ATLAS_REFRESHER,
    DEVELOPER,
    SCOUT,
    TEAM_PLANNER,
    VALIDATOR,
    register_all,
)


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
        assert defn.tool_call_limit == 50
