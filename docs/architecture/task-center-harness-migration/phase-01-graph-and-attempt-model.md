# Phase 01 - Graph and Attempt Model

## Goal

Introduce the durable state model required by the new harness shape before
orchestrator behavior is migrated.

This phase is mostly schema, persistence, and typed runtime state. It should
not change the high-level execution behavior until Phase 02 starts using the
new model.

## Durable entities

### `HarnessGraph`

A `HarnessGraph` is a scoped goal. It owns ordered Attempts and carries graph
lineage.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `id` | Graph id. |
| `parent_harness_graph_id` | Containment parent. Null only for `G_root`. |
| `prior_graph_id` | Partial-plan segment predecessor. Set only on `CONTINUE_AFTER_PARTIAL_PLAN`. |
| `spawn_reason` | `ROOT`, `REQUEST_PLAN`, or `CONTINUE_AFTER_PARTIAL_PLAN`. |
| `retry_budget` | Maximum Attempts this graph may use. |
| `status` | Graph lifecycle state, including open and terminal states. |
| `current_attempt_id` | Latest running Attempt for non-root graphs. |

`prior_graph_id` is not a retry chain. It is only for partial-plan
continuation lineage.

### `Attempt`

An `Attempt` is one full pass at a graph goal.

```
Attempt {
    attempt_index:        N
    prior_attempt_id:     id of Attempt N-1, null on first
    stage:                planning | generating | evaluating | closed
    planner_task_id:      uuid
    dag_edges:            [(from, to), ...]
    generator_task_ids:   [executor_1, verifier, ...]
    evaluator_task_id:    uuid
    status:               running | passed | failed
    fail_reason:          null
                        | planner_step_budget_exhausted
                        | generator_failed
                        | evaluator_failed
}
```

Per-Attempt evidence such as task summaries, planner scratchpads, and artifact
references belongs to the context engine. The harness model stores only the
structural state needed for lifecycle decisions.

## Root graph

`G_root` is special:

- It is created at session start.
- It has `spawn_reason = ROOT`.
- It has `retry_budget = 0`.
- It has no Attempt rows.
- It owns the root generator/executor task.
- Closing it ends the session.

## Spawn reasons and lineage

Every non-root graph is created by the orchestrator of the graph that becomes
its parent.

| Spawn reason | Trigger | Spawned by | `parent_harness_graph_id` | `prior_graph_id` |
| ------------ | ------- | ---------- | ------------------------- | ---------------- |
| `REQUEST_PLAN` | Executor calls `submit_request_plan(note)` | Orchestrator of executor's graph | graph containing the executor | null |
| `CONTINUE_AFTER_PARTIAL_PLAN` | Latest Attempt passed with `plan_shape = partial` | Orchestrator of completing graph | completed graph | completed graph |

Planner launch context for the new graph is composed by the context engine.
The harness only owns the lineage edges.

## Lineage semantics

Two upward walks coexist:

- `parent_harness_graph_id`: containment hierarchy. Used by orchestrator
  ownership and close-report bubble-up.
- `prior_graph_id`: partial-plan segment chain. Used by recursive
  partial-plan gating and continuation context.

The chains overlap for `CONTINUE_AFTER_PARTIAL_PLAN` edges. They diverge at
each `REQUEST_PLAN` boundary, where `prior_graph_id` resets.

## Retry budget

`HarnessGraph.retry_budget` is set at graph creation and is not inherited
across graphs.

| Graph creation | `retry_budget` |
| -------------- | -------------- |
| `G_root` | `0`; root executor has no retry |
| `REQUEST_PLAN` child | freshly configured default or note override |
| `CONTINUE_AFTER_PARTIAL_PLAN` child | freshly configured default or continuation override |

`attempts_used` is `len(graph.attempts)`.

Partial-plan continuation does not inherit prior segments' Attempt counts.
Each segment is its own graph with its own budget.

## Implementation tasks

1. Add or adapt typed models for `HarnessGraph` and `Attempt`.
2. Add persistence fields for graph lineage, spawn reason, retry budget,
   current Attempt, Attempt stage, and Attempt failure reason.
3. Scope planner, generator, verifier, and evaluator task ids to an Attempt.
4. Keep root executor storage outside the Attempt model.
5. Add repository/store helpers for:
   - creating root graph,
   - creating child graph,
   - creating next Attempt,
   - loading current Attempt for a graph,
   - walking `parent_harness_graph_id`,
   - walking `prior_graph_id`.
6. Backfill or compatibility-map existing task graph state as needed.

## Phase exit criteria

- The runtime can create and load `G_root`.
- The runtime can create a non-root graph with Attempt 1.
- Tests cover `REQUEST_PLAN` lineage reset.
- Tests cover `CONTINUE_AFTER_PARTIAL_PLAN` lineage extension.
- Tests prove retry creates another Attempt in the same graph, not a child
  graph.
