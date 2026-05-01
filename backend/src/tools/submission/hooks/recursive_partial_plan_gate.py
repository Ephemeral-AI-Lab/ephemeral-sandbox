"""Prehook blocking partial plans below partial-planned ancestor graphs.

The walking logic now lives in :mod:`task_center.harness_graph.ancestry`.
Both the legacy ``request_has_partial_plan_ancestor`` helper and the
``PartialPlanAncestorGate`` prehook are one-line shims around the canonical
implementation so resolvers, predicates, and prehooks all share one function
object — a property pinned by structural tests via ``inspect.unwrap``.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from pydantic import BaseModel

from task_center.complex_task.request import ComplexTaskRequest
from task_center.exceptions import GraphInvariantViolation
from task_center.harness_graph.ancestry import has_partial_planned_caller_ancestor
from task_center.harness_graph.runtime import HarnessGraphRuntime
from tools.core.context import ToolExecutionContextService
from tools.core.hooks import HookResult
from tools.submission.context import (
    HarnessSubmissionContextError,
    resolve_harness_submission_context,
)


def request_has_partial_plan_ancestor(
    request: ComplexTaskRequest,
    runtime: HarnessGraphRuntime,
) -> bool:
    """Back-compat shim — delegates to the canonical ancestry function."""
    return has_partial_planned_caller_ancestor(
        request_id=request.id,
        request_store=runtime.request_store,
        segment_store=runtime.segment_store,
        graph_store=runtime.graph_store,
        task_store=runtime.task_store,
    )


@dataclass(frozen=True, slots=True)
class PartialPlanAncestorGate:
    target_tool: str = "submit_partial_plan"

    async def run(
        self,
        tool_input: BaseModel,
        context: ToolExecutionContextService,
    ) -> HookResult[Any]:
        try:
            submission_context = resolve_harness_submission_context(context)
        except HarnessSubmissionContextError as exc:
            return HookResult.fail(str(exc))

        try:
            has_partial_ancestor = request_has_partial_plan_ancestor(
                submission_context.request,
                submission_context.runtime,
            )
        except GraphInvariantViolation as exc:
            return HookResult.fail(str(exc))

        if has_partial_ancestor:
            return HookResult.fail(
                "submit_partial_plan is disabled for this request because an "
                "ancestor complex-task request was spawned from a partial-planned "
                "harness graph. Submit a full plan for the current request."
            )
        return HookResult.pass_(tool_input)
