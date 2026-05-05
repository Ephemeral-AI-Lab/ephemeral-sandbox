"""Role and ownership gate for harness graph terminal tools."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from pydantic import BaseModel

from task_center.attempt.runtime import HarnessGraphRuntime
from task_center.task import HarnessTaskRole
from tools.core.context import ToolExecutionContextService
from tools.core.hooks import HookResult


@dataclass(frozen=True, slots=True)
class HarnessRoleGate:
    target_tool: str
    expected_role: HarnessTaskRole

    async def run(
        self,
        tool_input: BaseModel,
        context: ToolExecutionContextService,
    ) -> HookResult[Any]:
        runtime = context.get("harness_graph_runtime")
        if not isinstance(runtime, HarnessGraphRuntime):
            return HookResult.fail(
                "Missing harness graph runtime for this TaskCenter submission."
            )
        task_id = str(context.get("task_center_task_id") or "")
        if not task_id or task_id.isspace():
            return HookResult.fail(
                "Missing TaskCenter task id for this submission."
            )
        task = runtime.task_store.get_task(task_id)
        if task is None:
            return HookResult.fail(f"TaskCenter task {task_id!r} was not found.")

        actual_role = str(task.get("role") or "")
        if actual_role != self.expected_role.value:
            return HookResult.fail(
                f"{self.target_tool} is only valid for "
                f"{self.expected_role.value} tasks."
            )

        # Generator-role tasks may be the graph-less entry executor; the
        # closed-graph check only applies when there's a graph.
        graph_id = str(task.get("task_center_harness_graph_id") or "")
        if self.expected_role != HarnessTaskRole.GENERATOR and not graph_id:
            return HookResult.fail(
                f"TaskCenter task {task_id!r} is not attached to a harness graph."
            )
        if graph_id:
            graph = runtime.graph_store.get(graph_id)
            if graph is None:
                return HookResult.fail(
                    f"HarnessGraph {graph_id!r} was not found."
                )
            if graph.is_closed:
                return HookResult.fail(
                    "This harness graph is already closed; terminal submissions are disabled."
                )
        return HookResult.pass_(tool_input)
