"""Team-mode-only context tools, bound to a specific TeamRun + WorkItem."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Callable, NamedTuple

from team.context.siblings import SiblingView

if TYPE_CHECKING:
    from team.run import TeamRun
    from team.types import WorkItem


class TeamContextTool(NamedTuple):
    name: str
    description: str
    callable: Callable[..., Any]


def build_team_context_tools(team_run: "TeamRun", wi: "WorkItem") -> list[TeamContextTool]:
    sibling_view = SiblingView(team_run.dispatcher, wi.id, team_run.artifacts)

    def team_get_project_context() -> dict[str, Any]:
        return team_run.project_context.to_dict()

    def team_list_siblings(status: str | None = None) -> list[dict[str, Any]]:
        return sibling_view.list(status=status)

    def team_files_changed_since_dispatch() -> list[dict[str, Any]]:
        entries = team_run.change_log.since(wi.started_at, exclude_work_item_id=wi.id)
        return [
            {
                "work_item_id": e.work_item_id,
                "agent_run_id": e.agent_run_id,
                "filepath": e.filepath,
                "timestamp": e.timestamp.isoformat(),
            }
            for e in entries
        ]

    return [
        TeamContextTool(
            "team_get_project_context",
            "Read the TeamRun's project-level context (goal, user request, notes).",
            team_get_project_context,
        ),
        TeamContextTool(
            "team_list_siblings",
            "List sibling WorkItems in the same TeamRun. Optional status filter.",
            team_list_siblings,
        ),
        TeamContextTool(
            "team_files_changed_since_dispatch",
            "Files other WorkItems have changed since this WorkItem started.",
            team_files_changed_since_dispatch,
        ),
    ]
