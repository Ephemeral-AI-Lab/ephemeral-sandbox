"""Partial parent executor routes a child planner to a restricted terminal set.

The entry-origin goal's first iteration submits a partial plan with
``deferred_goal_for_next_iteration``. Its executor then requests a child goal. Because the
child goal's parent task belongs to that partial-planned attempt, the child
planner must launch as ``planner`` without a defer terminal. The entry-origin
continuation iteration still launches ``planner`` with both planner terminals.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from tools.submission.evaluator import submit_evaluation_success
from tools.submission.verifier import (
    submit_verification_success,
)
from tools.submission.planner import (
    submit_plan_closes_goal,
    submit_plan_defers_goal,
)

from task_center_runner.audit.events import EventType
from task_center_runner.scenarios._scenario_helpers import (
    is_recursive_workflow,
    minimal_full_plan,
    preflight_full_plan,
)
from task_center_runner.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


_CHILD_PACKAGE_ID = "partial_parent_child"
_CHILD_GOAL = (
    "Resolve the delegated child goal requested by an executor whose parent "
    "attempt submitted a partial plan."
)
_CONTINUATION_GOAL = (
    "Run the entry-origin follow-up iteration after the delegated child goal has "
    "returned its close report."
)


def _entry_origin_defers_plan() -> dict[str, Any]:
    return {
        "plan_spec": (
            "Execute the first entry-origin slice by delegating one oversized branch to "
            "a child goal, then continue the entry-origin goal afterward."
        ),
        "evaluation_criteria": [
            "The child goal is requested from the parent executor task.",
            "The parent observes the child goal close report before evaluation.",
        ],
        "tasks": [
            {"id": "delegate_child", "agent_name": "executor", "deps": []},
            {
                "id": "recursive_return_guard",
                "agent_name": "verifier",
                "deps": ["delegate_child"],
            },
        ],
        "task_specs": {
            "delegate_child": (
                f"ACTION request_recursive_workflow package={_CHILD_PACKAGE_ID}"
            ),
            "recursive_return_guard": "VERIFY checkpoint=recursive_return",
        },
        "deferred_goal_for_next_iteration": _CONTINUATION_GOAL,
    }


def _child_full_plan() -> dict[str, Any]:
    return minimal_full_plan(
        plan_spec=(
            "Run a full child workflow preflight to prove the delegated workflow "
            "cannot emit another partial plan."
        ),
        evaluation_criteria=[
            "The child workflow completes through a full plan.",
        ],
        task_id="child_reconcile",
        task_spec=(
            "ACTION recursive_reconcile slice=full_only_planner. Write the "
            "standard recursive close report for the parent verifier."
        ),
    )


class DeferredParentPlannerTerminalRouting(ScenarioBase):
    """Child goal from a partial parent gets the restricted planner terminals."""

    name = "pipeline.deferred_parent_planner_terminal_routing"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_DEFERS_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.RECURSIVE_WORKFLOW_REQUESTED,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_SUCCESS,
        EventType.VERIFIER_INVOKED,
        EventType.RECURSIVE_WORKFLOW_COMPLETED,
        EventType.VERIFIER_SUCCESS,
        EventType.EVALUATOR_SUCCESS,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if is_recursive_workflow(ctx):
            return ToolCallSpec(submit_plan_closes_goal, _child_full_plan())
        if ctx.iteration.sequence_no == 1:
            return ToolCallSpec(submit_plan_defers_goal, _entry_origin_defers_plan())
        return ToolCallSpec(
            submit_plan_closes_goal,
            preflight_full_plan(
                plan_spec=(
                    "Run the entry-origin continuation follow-up as a normal full plan."
                ),
                evaluation_criteria=(
                    "The entry-origin continuation iteration completed as a full plan.",
                ),
            ),
        )

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        context_message = ctx.context_message or ""
        if "request_recursive_workflow" in context_message:
            return (f"request_recursive_workflow:{_CHILD_PACKAGE_ID}",)
        if "ACTION recursive_" in context_message:
            return ("recursive_step",)
        return ("preflight",)

    def verifier_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_verification_success,
            {
                "summary": "Recursive child close report reached the parent.",
                "checks": ["recursive_return"],
            },
        )

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": "Planner routing scenario branch passed.",
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )

    def recursive_handoff_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return _CHILD_GOAL


__all__ = ["DeferredParentPlannerTerminalRouting"]
