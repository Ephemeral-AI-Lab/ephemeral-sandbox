"""Coordination planner toolkit — agent discovery and phase context queries."""

from tools.base import BaseToolkit
from tools.coordination_planner.list_agents_tool import make_list_agents_tool
from tools.coordination_planner.phase_context_tool import (
    make_list_phases_tool,
    make_query_phase_context_tool,
)


class CoordinationPlannerToolkit(BaseToolkit):
    """Coordinator's introspection toolkit — agents and phase context."""

    def __init__(
        self,
        *,
        agent_names: list[str] | None = None,
        phase_outputs: dict[str, dict] | None = None,
    ) -> None:
        super().__init__(
            name="coordination_planner",
            description="Agent discovery and planning phase context queries",
            tools=[
                make_list_agents_tool(agent_names=agent_names),
                make_query_phase_context_tool(phase_outputs=phase_outputs or {}),
                make_list_phases_tool(phase_outputs=phase_outputs or {}),
            ],
            instructions=(
                "Introspection tools for the coordination planner. "
                "Use to discover available agents and review outputs from prior phases.\n\n"
                "- `list_agents` — list all registered worker agents. "
                "Use to decide which agents to assign tasks to.\n"
                "- `query_phase_context` — read the output of a completed phase. "
                "Use to build on prior results when planning the next phase.\n"
                "- `list_phases` — list all phases and their status. "
                "Use to understand overall progress and dependencies."
            ),
        )


__all__ = ["CoordinationPlannerToolkit"]
