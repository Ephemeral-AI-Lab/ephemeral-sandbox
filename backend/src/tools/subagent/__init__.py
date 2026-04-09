"""Subagent toolkit — spawn focused worker subagents."""

from __future__ import annotations

from tools.core.base import BaseToolkit
from tools.subagent.run_subagent_tool import run_subagent


class SubagentToolkit(BaseToolkit):
    """Spawn focused worker subagents that run as background tasks."""

    def __init__(self) -> None:
        super().__init__(
            name="subagent",
            description="Spawn focused worker subagents.",
            tools=[run_subagent],
            instructions=(
                "Use `run_subagent` to delegate bounded work to a subagent.\n"
                "- Each call returns a `task_id` immediately; workers always run in the background.\n"
                "- Emit multiple `run_subagent` calls in one turn to fan out in parallel.\n"
                "- After spawning a worker, keep doing disjoint foreground work or launch other independent workers. Do not immediately block on the new task unless its result is the only remaining blocker.\n"
                "- Only subagent-typed targets are valid. In team mode, `team_planner`, `atlas_builder`, and `atlas_refresher` may launch only `scout`; they must not launch `developer` or `validator` here.\n"
                "- Prefer `check_background_progress(task_id=...)` to inspect a running worker before you wait on it.\n"
                "- Use `wait_for_background_task(task_id=...)` to join a worker when you are ready for its final answer.\n"
                "- Cancel stale or low-value workers with `cancel_background_task(task_id=...)`.\n"
                "- Workers cannot spawn subagents or launch their own background tasks."
            ),
        )


__all__ = ["SubagentToolkit"]
