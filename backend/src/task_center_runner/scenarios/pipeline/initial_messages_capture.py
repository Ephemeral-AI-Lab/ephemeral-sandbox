"""Scenario that exercises the complex composer paths whose initial
messages the matching report (``docs/reports/initial_messages_report.md``)
captures. Pre-Round-3 this captured exactly three rows per main agent;
Round 3 grew planners to four rows (row 4 = skill composite), so the
name was generalized to "initial messages."

Combines three orthogonal composer branches into one live run so a single
``message.jsonl`` tree carries every variant we want to inspect:

1. **Attempt retry** — iteration 1 attempt 1's planner submits a valid full
   plan, the executor runs the assigned task, and the evaluator returns
   ``submit_evaluation_failure``. Attempt 2 then sees a fully-populated
   ``<iteration status="current">`` / ``<attempt status="prior"
   verdict="fail">`` block in its planner context: real ``<plan_spec>``,
   per-task ``<status_summary>`` + ``<task>`` summaries, plus
   ``<evaluator_summary>`` and ``<failed_criteria>`` (no enclosing
   ``<generator_outcomes>`` or ``<evaluator_judgment>`` wrappers).
   Attempt 2 then submits a partial plan (handoff) to drive the
   continuation branch (#2 below).

2. **Continuation goal** — iteration 1 attempt 2 submits a *partial* plan
   with a ``deferred_goal_for_next_iteration``. The iteration coordinator spawns
   iteration 2 with ``creation_reason=DEFERRED_GOAL_CONTINUATION``; iteration 2's
   planner sees a ``<iteration iteration_no="1" status="prior">`` group
   with the accepted plan and summary.

3. **Executor launch coverage** — both iterations include at least one
   generator task per attempt. The single-task plans keep executor captures
   focused on the composer's context shape rather than tool chatter.

This scenario does NOT trigger advisor / subagent calls because the mock
runner does not currently invoke them — those initial-message
captures are produced programmatically by
``scripts/build_initial_messages_report.py``, which calls the real
builder functions in ``tools/ask_helper/_lib/_compose.py`` and
``task_center/task_guidance/builders.py`` (specifically
``build_explorer_task_guidance`` for the subagent's row-2 prose).
Adding a helper/subagent dispatch branch to ``MockSquadRunner`` is left
as a follow-up; the matching scenario hook is the
``call_helpers_in_executor`` flag below, which the runner can grow into
later.

Wire shape (see ``docs/reports/initial_messages_cases/README.md``):

* system + ``<context>`` envelope + ``<Task Guidance>`` envelope + skill
  row for planner / executor / evaluator launches (4 rows each — skills
  carry operational heuristics; ``<Task Guidance>`` carries the
  deterministic outline + role directive).
"""

from __future__ import annotations

from collections.abc import Sequence

from tools.submission.evaluator import (
    submit_evaluation_failure,
    submit_evaluation_success,
)
from tools.submission.planner import (
    submit_plan_closes_goal,
    submit_plan_defers_goal,
)

from task_center_runner.audit.events import EventType
from task_center_runner.scenarios._scenario_helpers import (
    preflight_full_plan,
    preflight_defers_plan,
)
from task_center_runner.scenarios.base import (
    ScenarioBase,
    ScenarioContext,
    ToolCallSpec,
)


_CONTINUATION_GOAL = (
    "Continue the initial-messages capture by running one more preflight "
    "in iteration 2 so the continuation planner sees prior iteration "
    "results."
)


class InitialMessagesCapture(ScenarioBase):
    """Continuation + attempt retry, single executor task per attempt.

    Iteration 1, attempt 1: planner submits a *valid* full plan; executor
    runs preflight; evaluator returns ``submit_evaluation_failure`` so the
    attempt is closed FAILED with rich, fully-rendered retry evidence.
    Iteration 1, attempt 2: planner sees that retry evidence in a
    ``<attempt status="prior" verdict="fail">`` block, submits a partial
    plan with a ``deferred_goal_for_next_iteration``; executor runs
    preflight; evaluator passes.
    Iteration 2, attempt 1: planner submits a full plan; executor runs
    preflight; evaluator passes; goal closes succeeded.
    """

    name = "pipeline.initial_messages_capture"

    # Bonus knob a future MockSquadRunner extension can read to invoke
    # ask_advisor / run_subagent inline from the executor task. Today it
    # is informational only.
    call_helpers_in_executor: bool = False

    expected_event_sequence: tuple[EventType, ...] = (
        # Iteration 1 attempt 1 — full submission then evaluator failure.
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_FAILURE,
        # Iteration 1 attempt 2 — planner submits partial plan after seeing
        # the rich failed-attempt block from attempt 1.
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_DEFERS_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_SUCCESS,
        # Iteration 2 — full plan after continuation.
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_SUCCESS,
    )

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if ctx.iteration.sequence_no == 1:
            if ctx.attempt.attempt_sequence_no == 1:
                # Valid full plan — driver for the evaluator-failure branch
                # in evaluator_response below. Attempt 2's planner will read
                # the resulting `<attempt status="failed">` block.
                return ToolCallSpec(
                    submit_plan_closes_goal, preflight_full_plan()
                )
            return ToolCallSpec(
                submit_plan_defers_goal,
                preflight_defers_plan(deferred_goal_for_next_iteration=_CONTINUATION_GOAL),
            )
        return ToolCallSpec(submit_plan_closes_goal, preflight_full_plan())

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:  # noqa: ARG002
        return ("preflight",)

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if (
            ctx.iteration.sequence_no == 1
            and ctx.attempt.attempt_sequence_no == 1
        ):
            # Intentional first-attempt evaluator failure so the next
            # planner's context carries a fully-populated
            # `<attempt status="prior" verdict="fail">` block (real
            # plan_spec, real per-task summaries, real evaluator
            # commentary). Without this the retry attempt would only see
            # the compact "bypassed" body emitted for planner-validation
            # failures.
            return ToolCallSpec(
                submit_evaluation_failure,
                {
                    "summary": (
                        "Intentional first-attempt evaluator failure to "
                        "exercise the rich failed-prior-attempt "
                        "retry-evidence rendering in the next attempt's "
                        "planner context."
                    ),
                    "failed_criteria": list(ctx.attempt.evaluation_criteria),
                },
            )
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": (
                    "Captured planner / executor / evaluator initial messages "
                    f"for iteration {ctx.iteration.sequence_no}."
                ),
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


__all__ = ["InitialMessagesCapture"]
