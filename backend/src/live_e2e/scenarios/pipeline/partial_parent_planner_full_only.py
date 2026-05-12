"""Partial parent executor routes a child planner to ``planner_full_only``.

The root mission's first episode submits a partial plan with
``continuation_goal``. Its executor then requests a child mission. Because the
child mission's parent task belongs to that partial-planned attempt, the child
planner must be selected through the ``planner`` agent.md variant and launch as
``planner_full_only``. The root continuation episode still launches the normal
``planner`` because it is not a child mission.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from tools.submission.main_agent.evaluator import submit_evaluation_success
from tools.submission.main_agent.generator.verifier import (
    submit_verification_success,
)
from tools.submission.main_agent.planner import (
    submit_full_plan,
    submit_partial_plan,
)

from live_e2e.audit.events import EventType
from live_e2e.scenarios._utils import (
    is_recursive_mission,
    minimal_full_plan,
    preflight_full_plan,
)
from live_e2e.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


_CHILD_PACKAGE_ID = "partial_parent_child"
_CHILD_GOAL = (
    "Resolve the delegated child mission requested by an executor whose parent "
    "attempt submitted a partial plan."
)
_CONTINUATION_GOAL = (
    "Run the root follow-up episode after the delegated child mission has "
    "returned its close report."
)


def _root_partial_plan() -> dict[str, Any]:
    return {
        "task_specification": (
            "Execute the first root slice by delegating one oversized branch to "
            "a child mission, then continue the root mission afterward."
        ),
        "evaluation_criteria": [
            "The child mission is requested from the parent executor task.",
            "The parent observes the child mission close report before evaluation.",
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
                f"ACTION request_recursive_mission package={_CHILD_PACKAGE_ID}"
            ),
            "recursive_return_guard": "VERIFY checkpoint=recursive_return",
        },
        "continuation_goal": _CONTINUATION_GOAL,
    }


def _child_full_plan() -> dict[str, Any]:
    return minimal_full_plan(
        task_specification=(
            "Run a full child-mission preflight to prove the delegated mission "
            "cannot emit another partial plan."
        ),
        evaluation_criteria=[
            "The child mission completes through a full plan.",
        ],
        task_id="child_reconcile",
        task_spec=(
            "ACTION recursive_reconcile slice=full_only_planner. Write the "
            "standard recursive close report for the parent verifier."
        ),
    )


class PartialParentPlannerFullOnly(ScenarioBase):
    """Child mission from a partial parent gets the full-only planner profile."""

    name = "pipeline.partial_parent_planner_full_only"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.ENTRY_EXECUTOR_INVOKED,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_PARTIAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.RECURSIVE_MISSION_REQUESTED,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_SUCCESS,
        EventType.VERIFIER_INVOKED,
        EventType.RECURSIVE_MISSION_COMPLETED,
        EventType.VERIFIER_SUCCESS,
        EventType.EVALUATOR_SUCCESS,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if is_recursive_mission(ctx):
            return ToolCallSpec(submit_full_plan, _child_full_plan())
        if ctx.episode.sequence_no == 1:
            return ToolCallSpec(submit_partial_plan, _root_partial_plan())
        return ToolCallSpec(
            submit_full_plan,
            preflight_full_plan(
                task_specification=(
                    "Run the root continuation follow-up as a normal full plan."
                ),
                evaluation_criteria=(
                    "The root continuation episode completed as a full plan.",
                ),
            ),
        )

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        task_input = ctx.task_input or ""
        if "request_recursive_mission" in task_input:
            return (f"request_recursive_mission:{_CHILD_PACKAGE_ID}",)
        if "ACTION recursive_" in task_input:
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

    def recursive_mission_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return _CHILD_GOAL


__all__ = ["PartialParentPlannerFullOnly"]
