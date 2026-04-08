"""Team-mode-only context tools, bound to a specific TeamRun + WorkItem."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Callable, NamedTuple

if TYPE_CHECKING:
    from team.run import TeamRun
    from team.types import WorkItem


class TeamContextTool(NamedTuple):
    name: str
    description: str
    callable: Callable[..., Any]


def build_team_context_tools(team_run: "TeamRun", wi: "WorkItem") -> list[TeamContextTool]:
    def team_get_project_context() -> dict[str, Any]:
        return team_run.project_context.to_dict()

    return [
        TeamContextTool(
            "team_get_project_context",
            "Read the TeamRun's project-level context (goal, user request, notes).",
            team_get_project_context,
        ),
    ]
