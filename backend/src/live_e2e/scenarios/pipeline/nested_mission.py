"""Recursive mission success and failure scenarios."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from tools.submission.evaluator import (
    submit_evaluation_failure,
    submit_evaluation_success,
)
from tools.submission.verifier import (
    submit_verification_success,
)
from tools.submission.planner import submit_full_plan

from live_e2e.audit.events import EventType
from live_e2e.scenarios._utils import is_recursive_mission
from live_e2e.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


def _root_nested_plan(*, failing_child: bool) -> dict[str, Any]:
    package_id = "child_failure" if failing_child else "child_success"
    return {
        "task_specification": "Delegate one oversized branch to a child mission.",
        "evaluation_criteria": [
            "Child mission is linked to the parent generator task.",
            "Parent graph does not finish before the child mission closes.",
        ],
        "tasks": [
            {"id": "delegate_child", "agent_name": "executor", "deps": []},
            {
                "id": "recursive_return_guard",
                "agent_name": "verifier",
                "deps": ["delegate_child"],
            },
            {
                "id": "parent_reconciliation",
                "agent_name": "executor",
                "deps": ["recursive_return_guard"],
            },
        ],
        "task_specs": {
            "delegate_child": f"ACTION request_recursive_mission package={package_id}",
            "recursive_return_guard": "VERIFY checkpoint=recursive_return",
            "parent_reconciliation": (
                "Run parent reconciliation after recursive close report."
            ),
        },
    }


def _child_success_plan() -> dict[str, Any]:
    return {
        "task_specification": "Execute a two-task child mission and close it.",
        "evaluation_criteria": [
            "Both child slices completed.",
            "Child close report can be delivered to the parent.",
        ],
        "tasks": [
            {"id": "child_a", "agent_name": "executor", "deps": []},
            {"id": "child_b", "agent_name": "executor", "deps": ["child_a"]},
        ],
        "task_specs": {
            "child_a": "ACTION recursive_execute slice=a",
            "child_b": "ACTION recursive_reconcile slice=b",
        },
    }


def _child_failure_plan() -> dict[str, Any]:
    return {
        "task_specification": "Child mission fails every attempt.",
        "evaluation_criteria": ["Parent receives a failed child close report."],
        "tasks": [
            {"id": "child_always_fails", "agent_name": "executor", "deps": []},
        ],
        "task_specs": {
            "child_always_fails": "ACTION child_failure reason=nested_mission",
        },
    }


class NestedMission(ScenarioBase):
    """Parent generator delegates to a child mission, then reconciles."""

    name = "pipeline.nested_mission"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.ENTRY_EXECUTOR_INVOKED,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.RECURSIVE_MISSION_REQUESTED,
        EventType.RECURSIVE_MISSION_COMPLETED,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if is_recursive_mission(ctx):
            return ToolCallSpec(submit_full_plan, _child_success_plan())
        return ToolCallSpec(submit_full_plan, _root_nested_plan(failing_child=False))

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        rendered_prompt = ctx.rendered_prompt or ""
        if "request_recursive_mission" in rendered_prompt:
            return ("request_recursive_mission:child_success",)
        if "ACTION recursive_" in rendered_prompt:
            return ("recursive_step",)
        return ("preflight",)

    def verifier_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_verification_success,
            {
                "summary": "Recursive return was observed by the parent.",
                "checks": ["recursive_return"],
            },
        )

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": "Nested mission completed before parent reconciliation.",
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )

    def recursive_mission_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return "Run the delegated child mission and return a close report."


class NestedMissionFailure(ScenarioBase):
    """Child mission exhausts attempts and parent mission fails cleanly."""

    name = "pipeline.nested_mission_failure"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.ENTRY_EXECUTOR_INVOKED,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.RECURSIVE_MISSION_REQUESTED,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if is_recursive_mission(ctx):
            return ToolCallSpec(submit_full_plan, _child_failure_plan())
        return ToolCallSpec(submit_full_plan, _root_nested_plan(failing_child=True))

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        rendered_prompt = ctx.rendered_prompt or ""
        if "request_recursive_mission" in rendered_prompt:
            return ("request_recursive_mission:child_failure",)
        if "child_failure" in rendered_prompt:
            return ("fail:Intentional child mission failure.",)
        return ("preflight",)

    def verifier_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_verification_success,
            {
                "summary": "Unexpected verifier reached after child failure.",
                "checks": ["unexpected"],
            },
        )

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_failure,
            {
                "summary": "Nested mission failure should not reach evaluator.",
                "failed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )

    def recursive_mission_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return "Run a child mission that intentionally exhausts attempts."


__all__ = ["NestedMission", "NestedMissionFailure"]
