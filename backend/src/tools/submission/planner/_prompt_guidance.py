"""Shared planner submission prompt guidance."""

from __future__ import annotations

from tools._names import (
    SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME,
    SUBMIT_PLAN_DEFERS_GOAL_TOOL_NAME,
)

PLAN_SUBMISSION_CHOICE_GUIDANCE = f"""\
## Close vs Defer Decision

Use `{SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME}` when:
- You estimate this attempt's generators and reducers are sufficient for the
  whole current goal.
- Sufficient means every required outcome is produced by the DAG and checked by
  reducers; after those reducers PASS, no known follow-up work remains.

Use `{SUBMIT_PLAN_DEFERS_GOAL_TOOL_NAME}` when:
- You estimate the current goal needs another planner pass after this bounded
  iteration runs.
- The current reducers gate and summarize this iteration. Once they pass, their
  outcomes become prior-iteration context for the next planner, alongside
  `deferred_goal_for_next_iteration`.
- The deferred goal is the next planner's scope; reducer outcomes are context
  for that planner, not a replacement for the deferred goal.

Do not submit either terminal yet when:
- The plan is uncertain, reducers are too broad, current-iteration work is
  unfinished, or the iteration boundary is unclear.
- You cannot state why reducer PASS means either goal completion
  (`{SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME}`) or a completed bounded iteration
  ready for the next planner (`{SUBMIT_PLAN_DEFERS_GOAL_TOOL_NAME}`).

Examples:
- Lane shape does not decide close vs defer. A sequence like
  `gen_a -> gen_b -> gen_c -> ...` can close or defer depending on whether
  another planner pass is needed.
- Use `{SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME}` when the reducers gate the complete
  result the current goal asked for.
- Use `{SUBMIT_PLAN_DEFERS_GOAL_TOOL_NAME}` when the reducers gate iteration
  outcomes that should become context for the next planner, and
  `deferred_goal_for_next_iteration` gives that planner a self-contained next
  goal.
"""

PLAN_DAG_GUIDANCE = """\
## Plan DAG Contract

A plan is a DAG of generator + reducer tasks. Generators do the work; reducers
digest their direct `needs` and gate the result. Use the smallest graph that
matches the context flow.

Rules:
- Root generators may have no `needs`.
- Non-root generator `needs` may reference one or more generator ids.
- Reducer `needs` must reference one or more generator ids.
- No task may need a reducer; reducers are terminal sinks.
- Every generator must be needed by another generator or by a reducer.

Context rule:
- `needs` are direct context inputs, not scheduling shortcuts.
- A task receives only the outcomes of ids listed in its own `needs`;
  transitive ancestors are not included.
- If `gen_b` needs `gen_a` and `gen_c` needs `gen_b`, then `gen_c` receives
  `gen_b` only.
- If `gen_c` also needs `gen_a`'s context, set
  `gen_c.needs = ["gen_a", "gen_b"]`.

Valid examples:

Overview graph:
   gen_a ----\\
              +--> gen_c ----\\
   gen_b ----/                +--> gen_e ----\\
             \\                /               +--> red_f
              +--> gen_d ----+--------------/
   gen_c -----------------------------------> red_g

1. One full serial lane:
   gen_a -> gen_b -> gen_c -> red_d

2. Multiple serial lanes:
   gen_a -> gen_b -> red_e
   gen_c -> gen_d -> red_f

3. Simple fan-in reducer:
   gen_a ----\\
   gen_b -----+--> red_d
   gen_c ----/

4. Diamond fork-join:
             +--> gen_b ----\\
   gen_a ----+                +--> gen_d ----> red_e
             +--> gen_c ----/

5. Tree:
   gen_a
   +--> gen_b ----\\
   |               +--> red_f
   +--> gen_c ----/
        +--> gen_d ----> red_g
        +--> gen_e ----> red_h

6. Fully-connected layers:
   gen_a ----+--> gen_c ----+--> red_e
             |              |
   gen_b ----+--> gen_d ----+--> red_f

7. Multi-phase mesh:
   gen_a ----> gen_c ----\\
                         +--> gen_e ----\\
   gen_b ----> gen_d ----/                +--> gen_h ----\\
                         +--> gen_f ----/                +--> red_i
   gen_c -----------------------------------------------> red_j
"""

__all__ = ["PLAN_DAG_GUIDANCE", "PLAN_SUBMISSION_CHOICE_GUIDANCE"]
