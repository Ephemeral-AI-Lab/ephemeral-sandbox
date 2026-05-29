"""Initial workflow, single attempt, single success.

Reference scenario for the simplest task_center happy path: TaskCenter entry
creates the initial workflow → planner emits one full plan → executor runs ``preflight`` →
evaluator passes → workflow closes succeeded. One workflow, one iteration
(``creation_reason=INITIAL``), one attempt (``attempt_sequence_no=1``).

Use this as the template for any "single-attempt success in a particular
configuration" scenario. Branch on ``ctx.iteration.sequence_no`` and
``ctx.attempt.attempt_sequence_no`` to cover more configurations.
"""

from __future__ import annotations

from collections.abc import Sequence

from tools.submission.evaluator import submit_evaluation_success
from tools.submission.planner import submit_plan_closes_goal

from task_center_runner.audit.events import EventType
from task_center_runner.scenarios._scenario_helpers import preflight_full_plan
from task_center_runner.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


class InitialWorkflow(ScenarioBase):
    """Single goal, single iteration, single attempt — happy path."""

    name = "pipeline.initial_workflow"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        return ToolCallSpec(submit_plan_closes_goal, preflight_full_plan())

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:  # noqa: ARG002
        return ("preflight",)

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": "Initial goal preflight evidence accepted.",
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


__all__ = ["InitialWorkflow"]
