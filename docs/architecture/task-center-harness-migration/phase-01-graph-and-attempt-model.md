# Phase 01 - Complex Task Request and Harness Graph Model

## Goal

Introduce the durable state model required by the new harness shape before
`ComplexTaskOrchestrator` and `HarnessGraphOrchestrator` behavior is migrated.

This phase is mostly schema, persistence, and typed runtime state. It should
not change high-level execution behavior until Phase 02 starts using the new
model.

## Durable entities

### `ComplexTaskRequest`

A `ComplexTaskRequest` is a complex delegated goal requested by an executor
that decided its assigned task is not atomic.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `id` | Complex task request id. |
| `run_id` | Owning TaskCenter run. |
| `requested_by_task_id` | Executor task that called `request_complex_task_solution`. |
| `goal` | Goal supplied by the executor. |
| `status` | `open`, `succeeded`, `failed`, or `cancelled`. |
| `current_segment_id` | Latest active `TaskSegment`. |
| `created_at` / `updated_at` / `closed_at` | Lifecycle timestamps. |

`requested_by_task_id` is the authoritative parent link for context and final
result routing.

### `TaskSegment`

A `TaskSegment` is one vertical slice of a complex task request. Segment 1
starts from the requested goal. Segment 2+ exists only when the previous
segment completed successfully with `plan_shape = partial`.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `id` | Segment id. |
| `complex_task_request_id` | Owning complex task request. |
| `previous_segment_id` | Previous partial-plan segment. Null for segment 1. |
| `sequence_no` | 1-based segment sequence in the request. |
| `creation_reason` | `initial` or `partial_continuation`. |
| `goal` | Segment goal or continuation instruction. |
| `retry_budget` | Maximum harness graph tries for this segment. |
| `status` | `open`, `succeeded`, `failed`, or `cancelled`. |
| `current_harness_graph_id` | Latest running harness graph for this segment. |

`previous_segment_id` is not a retry chain. It is only for partial-plan
continuation lineage.

### `HarnessGraph`

A `HarnessGraph` is one full planner-produced graph execution for one segment.
This is the retryable unit that runs `planner -> generator DAG -> evaluator`.

```
HarnessGraph {
    segment_id:          owning TaskSegment
    retry_no:            1 for initial try, 2+ for retry
    creation_reason:     initial | retry_after_failure
    stage:               planning | generating | evaluating | closed
    planner_task_id:     uuid
    generator_task_ids:  [executor_1, verifier, ...]
    evaluator_task_id:   uuid
    status:              running | passed | failed
    plan_shape:          null | full | partial
    fail_reason:         null
                       | planner_step_budget_exhausted
                       | generator_failed
                       | evaluator_failed
}
```

Per-harness-graph evidence such as task summaries, planner scratchpads, and
artifact references belongs to the context engine. The harness model stores
only the structural state needed for lifecycle decisions.

Generator ordering and dependency constraints live on task records rather than
on `HarnessGraph`.

There is no `ROOT` spawn or creation reason.

## Creation reasons and lineage

| Entity | Creation reason | Trigger | Parent / lineage |
| ------ | --------------- | ------- | ---------------- |
| `ComplexTaskRequest` | implicit complex-task request | Executor calls `request_complex_task_solution(goal)` | `requested_by_task_id` points to the executor. |
| `TaskSegment` | `initial` | Complex task request starts | `previous_segment_id = null`. |
| `TaskSegment` | `partial_continuation` | Prior segment's current harness graph passed with `plan_shape = partial` | `previous_segment_id` points to the prior segment. |
| `HarnessGraph` | `initial` | Segment starts | `retry_no = 1`. |
| `HarnessGraph` | `retry_after_failure` | Previous harness graph failed and segment retry budget remains | Same segment, `retry_no = previous + 1`. |

Retry is never a `ComplexTaskRequest` or `TaskSegment` creation reason.

## Context walks

Three context walks coexist:

- Request origin: `ComplexTaskRequest.requested_by_task_id`.
- Vertical continuation: `TaskSegment.previous_segment_id`.
- Horizontal retry: `HarnessGraph.segment_id` plus lower `retry_no` values.

The context engine can compose these into:

```text
ComplexTaskRequest
  goal = goal from requesting executor
  |
  +-- TaskSegment 1
  |     |
  |     +-- HarnessGraph 1
  |     |     initial try
  |     |
  |     +-- HarnessGraph 2
  |     |     retry after failure
  |     |
  |     `-- segment closes with plan_shape = partial
  |           because Segment 1 is partial,
  |           ComplexTaskOrchestrator creates TaskSegment 2
  |
  +-- TaskSegment 2
  |     |
  |     +-- HarnessGraph 1
  |     |     initial try
  |     |
  |     +-- HarnessGraph 2
  |     |     retry after failure
  |     |
  |     `-- segment closes with plan_shape = partial
  |           because Segment 2 is partial,
  |           ComplexTaskOrchestrator creates TaskSegment 3
  |
  +-- TaskSegment 3
  |     |
  |     +-- HarnessGraph 1
  |     |     final full plan
  |     |
  |     `-- segment closes with plan_shape = full
  |
  `-- ComplexTaskRequest closes and reports back to requested_by_task_id
```

## Retry budget

`TaskSegment.retry_budget` is set at segment creation. It may come from a
runtime default, request-level configuration, or continuation override, but it
is applied segment-locally.

`harness_graphs_used` is the count of harness graphs for that segment.

Partial-plan continuation does not inherit prior segments' retry count. Each
segment has its own budget.

## Complex Task Orchestrator

Add a `ComplexTaskOrchestrator` responsible for all structural creation.
Runtime handlers and `HarnessGraphOrchestrator`s should not manually assemble
`ComplexTaskRequest`, `TaskSegment`, or `HarnessGraph` records.

The `ComplexTaskOrchestrator` API should include:

| Method | Responsibility |
| ------ | -------------- |
| `create_complex_task_request(...)` | Create the request from `request_complex_task_solution`, set `requested_by_task_id`, store the goal, and initialize request status. |
| `create_initial_segment(...)` | Create segment 1 with `previous_segment_id = null`, set retry budget, and attach it to the request. |
| `create_continuation_segment(...)` | Create segment N+1 only after segment N closes with `plan_shape = partial`; set `previous_segment_id` and sequence number. |
| `create_initial_harness_graph(...)` | Create retry 1 for a segment and set it as `current_harness_graph_id`. |
| `create_retry_harness_graph(...)` | Create retry N+1 in the same segment after a harness graph failure and retry-budget check. |

`ComplexTaskOrchestrator` must enforce these invariants:

- Segment 1 is the only segment without `previous_segment_id`.
- Segment N+1 can only be created from a closed partial segment.
- Retry harness graphs stay in the same segment.
- Retry numbers are contiguous within a segment.
- A request, segment, or graph is initialized in exactly one valid opening
  state.

## Implementation tasks

1. Add or adapt typed models for `ComplexTaskRequest`, `TaskSegment`, and
   `HarnessGraph`.
2. Add persistence fields for request origin, segment lineage, retry budget,
   current segment, current harness graph, harness graph stage, plan shape, and
   failure reason.
3. Scope planner, generator, verifier, and evaluator task ids to a
   `HarnessGraph`.
4. Add `ComplexTaskOrchestrator` as the only creator of complex task,
   segment, and harness graph records.
5. Add repository/store helpers used by `ComplexTaskOrchestrator` for:
   - inserting a complex task request,
   - inserting the initial task segment,
   - inserting a partial-continuation task segment,
   - inserting the next retry harness graph,
   - loading the current segment for a request,
   - loading the current harness graph for a segment,
   - walking `requested_by_task_id`,
   - walking `previous_segment_id`,
   - listing harness graphs by segment and retry order.
6. Backfill or compatibility-map existing graph-as-attempt state as needed.

## Phase exit criteria

- The runtime can create and load a `ComplexTaskRequest`.
- The runtime can create segment 1 with harness graph retry 1.
- Tests cover `request_complex_task_solution` creating a request linked to
  `requested_by_task_id`.
- Tests cover partial continuation creating `TaskSegment` N+1 with
  `previous_segment_id`.
- Tests prove retry creates another `HarnessGraph` in the same segment, not a
  new segment or request.
