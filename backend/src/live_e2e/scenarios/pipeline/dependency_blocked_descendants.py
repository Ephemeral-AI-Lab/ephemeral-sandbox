"""Failed ancestor blocks downstream generator descendants."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from tools.submission.evaluator import submit_evaluation_failure
from tools.submission.planner import submit_full_plan

from live_e2e.audit.events import EventType
from live_e2e.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


def _blocked_descendants_plan() -> dict[str, Any]:
    return {
        "task_specification": (
            "Fail root task a and prove descendants b, c, and d never launch."
        ),
        "evaluation_criteria": [
            "Downstream descendants of failed task a were marked blocked.",
            "No evaluator launched for the failed generator stage.",
        ],
        "tasks": [
            {"id": "a", "agent_name": "executor", "deps": []},
            {"id": "b", "agent_name": "executor", "deps": ["a"]},
            {"id": "c", "agent_name": "executor", "deps": ["a"]},
            {"id": "d", "agent_name": "executor", "deps": ["b", "c"]},
        ],
        "task_specs": {
            "a": "ACTION fail_root reason=blocked_descendants",
            "b": "This task must remain blocked by a.",
            "c": "This task must remain blocked by a.",
            "d": "This fan-in task must remain blocked by b and c.",
        },
    }


class DependencyBlockedDescendants(ScenarioBase):
    """Failed root blocks descendants until the attempt fails."""

    name = "pipeline.dependency_blocked_descendants"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.ENTRY_EXECUTOR_INVOKED,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_FAILURE,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_FAILURE,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        return ToolCallSpec(submit_full_plan, _blocked_descendants_plan())

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        if "ACTION fail_root" in (ctx.rendered_prompt or ""):
            return ("fail:Intentional root failure for blocked-descendant coverage.",)
        return ("preflight",)

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_failure,
            {
                "summary": "Unexpected evaluator invocation after blocked descendants.",
                "failed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


__all__ = ["DependencyBlockedDescendants"]
