"""Subagent toolkit — parallel agent dispatch over work items."""

from __future__ import annotations

from tools.base import BaseToolkit
from tools.subagent.parallel_dispatch_tool import AgentRunFn, make_run_parallel_agents_tool


class SubagentToolkit(BaseToolkit):
    """Parallel agent dispatch — fan out work items to worker agents."""

    def __init__(self, *, run_agent_fn: AgentRunFn | None = None) -> None:
        super().__init__(
            name="subagent",
            description="Parallel agent dispatch: fan out work items to worker agents",
            tools=[make_run_parallel_agents_tool(run_agent_fn=run_agent_fn)],
            instructions=(
                "Dispatch independent work items to parallel worker agents. "
                "Use when a task can be decomposed into subtasks that don't depend on each other.\n\n"
                "- `run_parallel_agents` — send a list of work items to worker agents. "
                "Each item runs in its own agent concurrently. "
                "Use for batch operations like editing multiple files, running tests across modules, "
                "or processing independent data items."
            ),
        )


__all__ = ["SubagentToolkit"]
