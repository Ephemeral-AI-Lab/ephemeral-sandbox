"""Description factory for submit_plan_defers_goal."""

from __future__ import annotations

from tools._names import SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME
from tools.submission.planner._prompt_guidance import (
    PLAN_DAG_GUIDANCE,
    PLAN_SUBMISSION_CHOICE_GUIDANCE,
)


def get_submit_plan_defers_goal_description() -> str:
    return f"""\
Submit a plan that delivers a bounded iteration toward the goal
and defers the remainder to a follow-up iteration.

## When to Use This Tool
- The full goal is too large or risky to complete safely in one
  iteration.
- You can articulate a bounded iteration that is independently valuable AND
  a clear `deferred_goal_for_next_iteration` describing what's left.

## When NOT to Use This Tool
- The full goal fits in one iteration — use `{SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME}`.
- You haven't decided what to defer — that's a planning signal, not a
  next-iteration boundary.

{PLAN_SUBMISSION_CHOICE_GUIDANCE}

## Continuation Contract
- The submitted plan must stand on its own. Its tasks and reducers deliver a
  finished iteration that closes the current iteration. The continuation is for
  additional work, not unfinished work in this graph.
- `deferred_goal_for_next_iteration` is the next iteration's whole scope, not
  a backlog dump or a diff against this attempt. Write it as a self-contained
  instruction for a fresh planner.
- If the remainder contains many independent items, choose one coherent,
  bounded next iteration and leave later remainder for that future planner to size.

{PLAN_DAG_GUIDANCE}

## Inputs
- `tasks`: one or more generator descriptors for THIS iteration. Each has `id`,
  `agent_name`, and `needs`. Use `executor` for `agent_name`. `needs` defaults
  to `[]`.
- `task_specs`: map of generator id to detailed, nonblank task spec. It must
  contain exactly the generator ids from `tasks`.
- `reducers`: one or more reducer descriptors. Each has `id`, nonempty `needs`,
  and nonblank `prompt`.
- `deferred_goal_for_next_iteration`: self-contained, nonblank instruction for
  the next iteration's bounded remainder.

Validation rejects cycles, unknown ids, reducer dependencies, reducers with no
generator inputs, extra or missing `task_specs`, and dangling generators with no
downstream generator or reducer consumer.

## Behavior
- Records the deferring plan. Once the reducers pass, the next iteration is
  spawned automatically from `deferred_goal_for_next_iteration`.\
"""
