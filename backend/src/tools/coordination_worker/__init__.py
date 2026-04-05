"""Coordination worker toolkit — replanning escalation for worker agents."""

from __future__ import annotations

from typing import Callable

from tools.base import BaseToolkit
from tools.coordination_worker.replan_tool import (
    ArtifactStore,
    ReplanHandler,
    make_request_replan_tool,
)


class CoordinationWorkerToolkit(BaseToolkit):
    """Worker escalation toolkit — request replanning when tasks hit issues."""

    def __init__(
        self,
        *,
        task_id: str = "",
        run_id: str = "",
        store: ArtifactStore | None = None,
        replan_handler: ReplanHandler | None = None,
        trigger_dispatch_fn: Callable[[], None] | None = None,
    ) -> None:
        super().__init__(
            name="coordination_worker",
            description="Worker escalation: request replanning when tasks encounter issues",
            tools=[
                make_request_replan_tool(
                    task_id=task_id,
                    run_id=run_id,
                    store=store,
                    replan_handler=replan_handler,
                    trigger_dispatch_fn=trigger_dispatch_fn,
                ),
            ],
            instructions=(
                "Escalation tool for worker agents in a coordinated pipeline. "
                "Use when your assigned task hits a blocker that requires replanning.\n\n"
                "- `request_replan` — signal the coordinator that your task cannot proceed. "
                "Include the reason and any partial results. "
                "The coordinator will reassess the plan and may reassign or restructure work."
            ),
        )


__all__ = ["CoordinationWorkerToolkit"]
