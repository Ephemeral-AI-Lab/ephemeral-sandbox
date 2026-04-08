"""Team-mode-only context tools, bound to a specific TeamRun + WorkItem.

These are plain callables — they don't inherit from ``tools.core.base.BaseTool``
because the Worker wires them straight into the ``QueryContext`` at dispatch
time, not through the global tool registry. That keeps team mode's tool
exposure strictly scoped to the currently executing WorkItem.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Callable

from team.context.siblings import SiblingView

if TYPE_CHECKING:
    from team.run import TeamRun
    from team.types import WorkItem


@dataclass
class TeamContextTool:
    name: str
    description: str
    callable: Callable[..., Any]


def build_team_context_tools(team_run: "TeamRun", wi: "WorkItem") -> list[TeamContextTool]:
    sibling_view = SiblingView(team_run.dispatcher, wi.id, team_run.artifacts)

    def team_get_project_context() -> dict[str, Any]:
        return team_run.project_context.to_dict()

    def team_list_siblings(status: str | None = None) -> list[dict[str, Any]]:
        return [
            {
                "work_item_id": s.work_item_id,
                "agent_name": s.agent_name,
                "status": s.status,
                "payload_summary": s.payload_summary,
                "artifact_summary": s.artifact_summary,
            }
            for s in sibling_view.list(status=status)
        ]

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
            name="team_get_project_context",
            description="Read the TeamRun's project-level context (goal, user request, notes).",
            callable=team_get_project_context,
        ),
        TeamContextTool(
            name="team_list_siblings",
            description="List sibling WorkItems in the same TeamRun. Optional status filter.",
            callable=team_list_siblings,
        ),
        TeamContextTool(
            name="team_files_changed_since_dispatch",
            description="Files other WorkItems have changed since this WorkItem started.",
            callable=team_files_changed_since_dispatch,
        ),
    ]
