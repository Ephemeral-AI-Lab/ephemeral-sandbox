# Phase 04 - Vertical Graph Spawning and Bubble-Up

## Goal

Implement vertical graph motion after the graph, Attempt, orchestrator, and
tool-gate foundations are in place.

Vertical motion creates new `HarnessGraph`s for new scopes. It is used only
for:

- executor delegation through `REQUEST_PLAN`,
- continuation after a successful partial plan through
  `CONTINUE_AFTER_PARTIAL_PLAN`.

## Child graph spawn paths

```
parent graph G_p
  |
  +-- REQUEST_PLAN
  |     executor in G_p calls submit_request_plan(note)
  |     G_p orchestrator spawns child G_c
  |     G_p stays open
  |     calling executor waits for G_c close report
  |
  +-- CONTINUE_AFTER_PARTIAL_PLAN
        G_p latest Attempt passed with plan_shape = partial
        G_p orchestrator reads continuation instructions
        G_p orchestrator spawns child G_c
        G_p stays open
        G_p adopts G_c final outcome when G_c closes
```

New child graph fields:

| Spawn reason | `parent_harness_graph_id` | `prior_graph_id` |
| ------------ | ------------------------- | ---------------- |
| `REQUEST_PLAN` | graph containing the executor | null |
| `CONTINUE_AFTER_PARTIAL_PLAN` | completed graph | completed graph |

## `REQUEST_PLAN` workflow

```
G1.A_current has generator/executor 7

executor 7 calls submit_request_plan(note)
    |
    v
G1.Orch spawns child G1.X
  parent_harness_graph_id = G1
  prior_graph_id          = null
  spawn_reason            = REQUEST_PLAN
    |
    v
G1.X runs its own Attempts to completion
    |
    v
G1.Orch delivers child_success or child_failure report
back to executor 7 inside G1.A_current
    |
    v
executor 7 resumes inside G1.A_current
```

`REQUEST_PLAN` may happen at any graph depth and during any Attempt. Gating
predicates that walk `prior_graph_id` reset at this boundary.

## Partial-plan continuation workflow

```
planner G1.A_k submits submit_partial_plan(
    dag,
    details,
    instructions_on_what_to_do_after_completion_of_partial_plan
)
    |
    v
G1.A_k runs its partial DAG
    |
    v
evaluator submits success
    |
    v
G1.Orch marks A_k passed with plan_shape = partial
    |
    v
G1.Orch spawns child G_next
  spawn_reason            = CONTINUE_AFTER_PARTIAL_PLAN
  parent_harness_graph_id = G1
  prior_graph_id          = G1
    |
    v
G_next.Orch starts Attempt 1
    |
    v
planner G_next.A1 sees prior_graph_id chain already contains partial
submit_partial_plan is gated; planner must submit_full_plan
```

`G1` stays open while `G_next` runs. When `G_next` closes, `G1` closes with
the same outcome and the close report bubbles to whoever requested `G1`.

## Bubble-up and close reports

A graph closes exactly once. Its outcome is the outcome of either:

- the latest Attempt that passed, or
- the last Attempt at the retry budget.

When child graph `G_c` closes, its orchestrator hands a close report to the
parent orchestrator. The harness-owned close report fields are:

| Field | Meaning |
| ----- | ------- |
| `harness_graph_id` | child graph id |
| `spawn_reason` | `REQUEST_PLAN` or `CONTINUE_AFTER_PARTIAL_PLAN` |
| `outcome` | `success` or `failed` |
| `plan_shape` | `full` or `partial` for the final Attempt when available |

Detailed payload such as per-task summaries, planner scratchpads, and
evidence links belongs to the context engine.

## Close-report routing

| Spawn reason | Routing |
| ------------ | ------- |
| `REQUEST_PLAN` | report returns to the executor task that called `submit_request_plan`; that executor resumes inside its own Attempt |
| `CONTINUE_AFTER_PARTIAL_PLAN` | parent graph adopts the child outcome and closes with the same outcome |

Retry never bubbles up. Retry is internal motion inside one graph. A parent
sees one child graph close once with one final outcome.

## Implementation tasks

1. Implement child graph creation for `REQUEST_PLAN`.
2. Pause and resume the calling executor around the child graph close report.
3. Reset `prior_graph_id` on `REQUEST_PLAN` children.
4. Implement child graph creation for `CONTINUE_AFTER_PARTIAL_PLAN`.
5. Set `prior_graph_id` on continuation children.
6. Keep parent graphs open while continuation children run.
7. Route continuation child close reports by adopting the child outcome.
8. Route request-plan child close reports back to the calling executor.
9. Add close-report persistence or delivery semantics robust enough for
   process restart if the surrounding runtime supports it.

## Phase exit criteria

- `REQUEST_PLAN` creates a child graph and resumes the calling executor after
  child close.
- `CONTINUE_AFTER_PARTIAL_PLAN` creates a child graph and gates recursive
  partial plans.
- Parent graphs close only after their continuation child closes.
- Retry still stays inside the same graph and does not produce close reports
  until graph closure.
