"""Attempt budget exhausted — every attempt fails, mission closes failed.

The default ``TaskCenterLifecycleConfig.default_attempt_budget`` is ``2``
(``backend/src/task_center/config.py:16``). This scenario plans a single
generator task that **always** calls ``submit_execution_failure``, so each
attempt closes ``status=failed``, ``fail_reason="generator_failed"``. After
attempt 2 fails, ``EpisodeManager.has_budget_remaining`` is False — episode
closes failed, mission handler closes the mission failed.

Asserts: 1 mission (status=failed), 1 episode (status=failed), exactly 2
attempts each with ``fail_reason=generator_failed``,
``EXECUTOR_FAILURE`` appears twice in the event sequence, and there is no
``EVALUATOR_INVOKED`` event in the entire run (evaluator never spawned
because the generator stage never reached quiescence-with-all-DONE).

This is the canonical "max-retry" coverage; reuse the pattern when adding
scenarios that exercise budget-exhaustion under planner_failed or
evaluator_failed retry paths.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from tools.submission.evaluator import submit_evaluation_failure
from tools.submission.planner import submit_full_plan

from live_e2e.audit.events import EventType
from live_e2e.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec


def _always_fail_plan() -> dict[str, Any]:
    return {
        "task_specification": (
            "Single generator task that intentionally fails every attempt "
            "to exercise the episode attempt-budget exhaustion path."
        ),
        "evaluation_criteria": [
            "Episode closes failed after the attempt budget is exhausted.",
        ],
        "tasks": [
            {"id": "always_fail", "agent_name": "executor", "deps": []},
        ],
        "task_specs": {
            "always_fail": "Intentionally fail this generator task.",
        },
    }


class AttemptBudgetExhausted(ScenarioBase):
    """Every attempt fails — budget exhaustion closes the mission failed."""

    name = "pipeline.attempt_budget_exhausted"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.ENTRY_EXECUTOR_INVOKED,
        # Attempt 1 — planner ok, executor fails, no evaluator.
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_FAILURE,
        # Attempt 2 — same outcome, budget exhausted after this.
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_FULL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_FAILURE,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        return ToolCallSpec(submit_full_plan, _always_fail_plan())

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:  # noqa: ARG002
        return (
            "fail:Intentional generator failure to exhaust the attempt budget.",
        )

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        # Should never be invoked — the generator stage never reaches
        # all-DONE quiescence, so the dispatcher never spawns the evaluator.
        # Implementation exists only to satisfy the protocol.
        return ToolCallSpec(
            submit_evaluation_failure,
            {
                "summary": "Unexpected evaluator invocation — no DAG ever DONE.",
                "failed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


__all__ = ["AttemptBudgetExhausted"]
