---
intent: read_only
terminal: true
hooks: [no_background_sessions, {disallow_nested_planner_deferral: {max_depth: 1}}, advisor_approval]
---
Terminate your planner run by submitting the generator/reducer DAG for this attempt.

## Inputs
- `tasks`: generator tasks, each with `id`, `agent_name`, and `needs`.
- `task_specs`: mapping from each generator task id to its executable task text.
- `reducers`: reducer tasks, each with `id`, one or more generator `needs`, and `prompt`.
- `deferred_goal_for_next_iteration`: optional concrete goal items from the
  current iteration goal that this plan intentionally leaves for the next
  iteration. Omit or null means this plan covers all current-iteration goal
  items and leaves no remaining items.

## Behavior
- With no deferred goal, the plan closes the current iteration once reducers pass.
- With a nonblank deferred goal, the plan completes this bounded iteration and
  carries those remaining current-iteration goal items into the next iteration.
- The attempt PASSES iff every plan task reaches DONE.

## Plan DAG Contract

A plan is a DAG of generator + reducer tasks. Generators do the work; reducers
work on assigned reducer tasks using their direct `needs` as context, then
report outcome summaries. Use the smallest graph that matches the context flow.

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
   gen_a ----\
              +---> gen_c ----\
   gen_b ----/                +---> gen_e ----\
             \                /               +---> red_f
              +---> gen_d ----+--------------/
   gen_c -----------------------------------> red_g

1. One full serial lane:
   gen_a -> gen_b -> gen_c -> red_d

2. Multiple serial lanes:
   gen_a -> gen_b -> red_e
   gen_c -> gen_d -> red_f

3. Simple fan-in reducer:
   gen_a ----\
   gen_b -----+---> red_d
   gen_c ----/

4. Diamond fork-join:
              +---> gen_b ----\
   gen_a ----+                +---> gen_d ----> red_e
              +---> gen_c ----/

5. Tree:
   gen_a
   +---> gen_b ----\
   |               +---> red_f
   +---> gen_c ----/
        +---> gen_d ----> red_g
        +---> gen_e ----> red_h

6. Fully-connected layers:
   gen_a ----+---> gen_c ----+---> red_e
             |              |
   gen_b ----+---> gen_d ----+---> red_f

7. Multi-phase mesh:
   gen_a ----> gen_c ----\
                         +---> gen_e ----\
   gen_b ----> gen_d ----/                +---> gen_h ----\
                         +---> gen_f ----/                +---> red_i
   gen_c -----------------------------------------------> red_j

## Close vs Defer Decision

Call `submit_planner_outcome` without `deferred_goal_for_next_iteration` when:
- This iteration's generator work and reducer outcomes are enough to finish the
  current iteration goal.
- The plan covers all current-iteration goal items and leaves no remaining
  items.

Call `submit_planner_outcome` with nonblank `deferred_goal_for_next_iteration` when:
- You have a concrete plan for a bounded subset of the current iteration goal,
  and the listed remaining current-iteration goal items should move to the next
  iteration after this iteration's reducer outcomes exist.
- The current plan is concrete; what would be speculative is planning the full
  goal beyond this iteration before those reducer outcomes exist.
- Once current reducers complete successfully, their outcomes become
  prior-iteration context for the next planner, alongside
  `deferred_goal_for_next_iteration`.
- The deferred goal is concrete remaining current-iteration goal items, not a
  generic backlog dump or unrelated future idea.

Examples:
- Lane shape does not decide close vs defer. A sequence like
  `gen_a -> gen_b -> gen_c -> ...` can close or defer depending on whether
  another planner pass is needed.
- Omit `deferred_goal_for_next_iteration` when the collection of reducer
  outcomes is sufficient for the current iteration goal and leaves no remaining
  items.
- Set `deferred_goal_for_next_iteration` when the reducers produce iteration
  outcomes that should become context for the next planner, and
  `deferred_goal_for_next_iteration` lists the remaining current-iteration goal
  items.