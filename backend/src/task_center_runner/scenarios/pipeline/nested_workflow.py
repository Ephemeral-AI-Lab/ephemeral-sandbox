"""Recursive goal success and failure scenarios."""

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
from tools.submission.planner import submit_plan_closes_goal

from task_center_runner.audit.events import EventType
from task_center_runner.scenarios._scenario_helpers import is_recursive_workflow
from task_center_runner.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


def _entry_origin_nested_plan(*, failing_child: bool) -> dict[str, Any]:
    package_id = "child_failure" if failing_child else "child_success"
    return {
        "plan_spec": "Delegate one oversized branch to a child goal.",
        "evaluation_criteria": [
            "Child goal is linked to the parent generator task.",
            "Parent graph does not finish before the child goal closes.",
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
            "delegate_child": f"ACTION request_recursive_workflow package={package_id}",
            "recursive_return_guard": "VERIFY checkpoint=recursive_return",
            "parent_reconciliation": (
                "Run parent reconciliation after recursive close report."
            ),
        },
    }


def _child_success_plan() -> dict[str, Any]:
    return {
        "plan_spec": "Execute a two-task child goal and close it.",
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
        "plan_spec": "Child goal fails every attempt.",
        "evaluation_criteria": ["Parent receives a failed child close report."],
        "tasks": [
            {"id": "child_always_fails", "agent_name": "executor", "deps": []},
        ],
        "task_specs": {
            "child_always_fails": "ACTION child_failure reason=nested_workflow",
        },
    }


class NestedWorkflow(ScenarioBase):
    """Parent generator delegates to a child goal, then reconciles."""

    name = "pipeline.nested_workflow"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.RECURSIVE_WORKFLOW_REQUESTED,
        EventType.RECURSIVE_WORKFLOW_COMPLETED,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if is_recursive_workflow(ctx):
            return ToolCallSpec(submit_plan_closes_goal, _child_success_plan())
        return ToolCallSpec(
            submit_plan_closes_goal,
            _entry_origin_nested_plan(failing_child=False),
        )

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        context_message = ctx.context_message or ""
        if "request_recursive_workflow" in context_message:
            return ("request_recursive_workflow:child_success",)
        if "ACTION recursive_" in context_message:
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
                "summary": "Nested goal completed before parent reconciliation.",
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )

    def recursive_handoff_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return "Run the delegated child goal and return a close report."


class NestedWorkflowFailure(ScenarioBase):
    """Child goal exhausts attempts and parent goal fails cleanly."""

    name = "pipeline.nested_workflow_failure"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.RECURSIVE_WORKFLOW_REQUESTED,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if is_recursive_workflow(ctx):
            return ToolCallSpec(submit_plan_closes_goal, _child_failure_plan())
        return ToolCallSpec(
            submit_plan_closes_goal,
            _entry_origin_nested_plan(failing_child=True),
        )

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        context_message = ctx.context_message or ""
        if "request_recursive_workflow" in context_message:
            return ("request_recursive_workflow:child_failure",)
        if "child_failure" in context_message:
            return ("fail:Intentional child goal failure.",)
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
                "summary": "Nested goal failure should not reach evaluator.",
                "failed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )

    def recursive_handoff_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return "Run a child goal that intentionally exhausts attempts."


__all__ = ["NestedWorkflow", "NestedWorkflowFailure"]
