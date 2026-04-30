# Phase 01 - Complex Task Request and Harness Graph Model

## Goal

Introduce the durable state model required by the new harness shape before
`ComplexTaskRequestHandler`, `TaskSegmentManager`, and
`HarnessGraphOrchestrator` behavior is migrated.

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
segment closed with a non-null `continuation_goal`.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `id` | Segment id. |
| `complex_task_request_id` | Owning complex task request. |
| `previous_segment_id` | Previous continuation segment. Null for segment 1. |
| `sequence_no` | 1-based segment sequence in the request. |
| `creation_reason` | `initial` or `partial_continuation`. |
| `goal` | Segment goal. For segment 1, this equals the request goal. For segment 2+, this equals the previous segment's `continuation_goal`. |
| `retry_budget` | Maximum harness graph tries for this segment. |
| `status` | `open`, `succeeded`, `failed`, or `cancelled`. |
| `current_harness_graph_id` | Latest running harness graph for this segment. |
| `continuation_goal` | Set when the segment closes from the passing harness graph that closed it (the last successful harness graph in the segment). Null while the segment is open. Null on terminal close (passing graph submitted a full plan) or on failure; non-null when the passing graph submitted a partial plan. |

`previous_segment_id` is not a retry chain. It is only for continuation
lineage.

### `HarnessGraph`

A `HarnessGraph` is one full planner-produced graph execution for one segment.
It runs `planner -> generator DAG -> evaluator`. Retry policy is not stored on
the graph; `TaskSegmentManager` decides whether a failed graph should be
followed by another graph in the same segment.

```
HarnessGraph {
    segment_id:          owning TaskSegment
    graph_sequence_no:   1-based graph order inside the segment
    stage:               planning | generating | evaluating | closed
    planner_task_id:     uuid
    task_specification:
                         string from submit_full_plan or submit_partial_plan
    evaluation_criteria:
                         [criterion, ...]
    generator_task_ids:  [executor_1, verifier, ...]
    evaluator_task_id:   null | uuid
    status:              running | passed | failed
    continuation_goal:   null
                       | string (set from submit_partial_plan)
    fail_reason:         null
                       | planner_step_budget_exhausted
                       | generator_failed
                       | evaluator_failed
}
```

Per-harness-graph evidence such as task summaries, planner scratchpads, and
artifact references belongs to the context engine. The harness model stores
only the structural state needed for lifecycle decisions.

`task_specification` and `evaluation_criteria` are the segment contract emitted
by the planner. `HarnessGraphOrchestrator` passes them to the evaluator as
evaluation instructions. The harness graph that passes closes its segment, and
its contract is the segment's record.

`continuation_goal` is set per-graph by that graph's own planner. A later
harness graph in the same segment does not inherit `continuation_goal` from a
prior failed graph; the new planner decides independently whether to submit a
full plan (null) or a partial plan (non-null). The segment's
`continuation_goal` is set only from the passing harness graph that closes
the segment.

Generator ordering and dependency constraints live on task records rather than
on `HarnessGraph`.

`evaluator_task_id` is unset while the graph is in `planning` or `generating`.
`HarnessGraphOrchestrator` creates the evaluator only after every generator
task in the current graph has completed successfully.

There is no `ROOT` spawn or creation reason.

## Creation reasons and lineage

| Entity | Creation reason | Trigger | Parent / lineage |
| ------ | --------------- | ------- | ---------------- |
| `ComplexTaskRequest` | implicit complex-task request | Executor calls `request_complex_task_solution(goal)` | `requested_by_task_id` points to the executor. |
| `TaskSegment` | `initial` | Complex task request starts | `previous_segment_id = null`. |
| `TaskSegment` | `partial_continuation` | Prior segment closed with non-null `continuation_goal` | `previous_segment_id` points to the prior segment. |
| `HarnessGraph` | none | Segment starts or `TaskSegmentManager` retries after failed graph | Same segment, `graph_sequence_no = previous + 1`. |

Retry is never a `ComplexTaskRequest`, `TaskSegment`, or `HarnessGraph`
creation reason. It is a `TaskSegmentManager` decision after a failed graph. A
passing harness graph closes its segment; it never produces another graph.

## Context walks

Three context walks coexist:

- Request origin: `ComplexTaskRequest.requested_by_task_id`.
- Vertical continuation: `TaskSegment.previous_segment_id` plus each prior
  segment's `continuation_goal`.
- Horizontal graph history: `HarnessGraph.segment_id` plus lower
  `graph_sequence_no` values. Retry context is derived from prior failed
  graphs by `TaskSegmentManager` and the context engine.

The context engine can compose these into:

```text
ComplexTaskRequest
  goal = goal from requesting executor
  |
  +-- TaskSegment 1
  |     |
  |     +-- HarnessGraph 1
  |     |     initial try, failed
  |     |
  |     `-- HarnessGraph 2
  |           second graph after failed graph, passed with continuation_goal != null
  |           segment 1 closes with continuation_goal inherited from HarnessGraph 2
  |           ComplexTaskRequestHandler creates TaskSegment 2
  |
  +-- TaskSegment 2
  |     |
  |     +-- HarnessGraph 1
  |     |     initial try, failed
  |     |
  |     `-- HarnessGraph 2
  |           second graph after failed graph, passed with continuation_goal != null
  |           segment 2 closes with continuation_goal inherited
  |           ComplexTaskRequestHandler creates TaskSegment 3
  |
  +-- TaskSegment 3
  |     |
  |     `-- HarnessGraph 1
  |           passed with continuation_goal = null
  |           segment 3 closes terminal
  |
  `-- ComplexTaskRequest closes and reports back to requested_by_task_id
```

## Segment retry policy

`TaskSegment.retry_budget` is set at segment creation. It may come from a
runtime default, request-level configuration, or continuation override, but it
is applied segment-locally.

`harness_graphs_used` is the count of harness graphs for that segment.

Continuation does not inherit prior segments' retry count. Each segment has
its own budget.

## Lifecycle Services

Add three lifecycle services. Runtime tool handlers and
`HarnessGraphOrchestrator`s should not manually assemble
`ComplexTaskRequest`, `TaskSegment`, or `HarnessGraph` records.

`ComplexTaskRequestHandler` owns the request boundary and the segment chain.
It is the only creator of `ComplexTaskRequest` and `TaskSegment` records:

| Method | Responsibility |
| ------ | -------------- |
| `create_complex_task_request(...)` | Create the request from `request_complex_task_solution`, set `requested_by_task_id`, store the goal, and initialize request status. |
| `create_initial_segment(...)` | Create segment 1 with `previous_segment_id = null` and `goal = request.goal`, set retry budget, attach it to the request, and spawn a `TaskSegmentManager` bound to that segment. |
| `create_continuation_segment(...)` | Create segment N+1 only after segment N closes with non-null `continuation_goal`; set `previous_segment_id`, `sequence_no = N+1`, and `goal = previous_segment.continuation_goal`; spawn a fresh `TaskSegmentManager` for the new segment. |
| `handle_segment_closed(...)` | Receive the `SegmentCloseReport` from the per-segment `TaskSegmentManager`; route `success_terminal` and `failed_exhausted` to request close, route `success_continue(goal)` to `create_continuation_segment`. |
| `close_complex_task_request(...)` | Store the final result and resume `requested_by_task_id` with the complex-task close report. |

`TaskSegmentManager` is per-`TaskSegment` and owns harness-graph transitions
inside that one segment. It is the only creator of `HarnessGraph` records:

| Method | Responsibility |
| ------ | -------------- |
| `create_initial_harness_graph(...)` | Create graph sequence 1 for the owned segment and set it as `current_harness_graph_id`. |
| `create_next_harness_graph(...)` | Create graph sequence N+1 in the same segment after a failed harness graph and segment retry-budget check. |
| `handle_harness_graph_closed(...)` | React to a graph outcome by either retrying inside the segment or closing the segment and emitting a `SegmentCloseReport` to `ComplexTaskRequestHandler`. |

`SegmentCloseReport` is the only signal from `TaskSegmentManager` to
`ComplexTaskRequestHandler`:

```
SegmentCloseReport {
  segment_id
  closing_harness_graph_id     # passing graph, or last failed graph if exhausted
  outcome ∈ {
    success_terminal,            # passing graph; continuation_goal is null
    success_continue(goal),      # passing graph; continuation_goal is non-null
    failed_exhausted(reason),    # retry budget exhausted on failed graph
  }
}
```

`TaskSegmentManager` must enforce these invariants:

- Subsequent harness graphs stay in the same segment.
- Graph sequence numbers are contiguous within a segment.
- A passing harness graph always closes the owned segment; it never produces a
  subsequent graph.
- A failed harness graph returns to `TaskSegmentManager`; the manager retries
  while budget remains, and closes the segment failed once budget is exhausted.
- The segment is initialized with exactly one initial harness graph and closes
  exactly once.
- The manager never creates `ComplexTaskRequest` or `TaskSegment` records.

`ComplexTaskRequestHandler` must enforce these invariants:

- Segment 1 is the only segment without `previous_segment_id`.
- Segment N+1 can only be created from a closed segment whose
  `continuation_goal` is non-null.
- Exactly one `TaskSegmentManager` instance is active per open segment.
- A request, segment, or graph is initialized in exactly one valid opening
  state.

## Implementation tasks

1. Add or adapt typed models for `ComplexTaskRequest`, `TaskSegment`, and
   `HarnessGraph`.
2. Add persistence fields for request origin, segment lineage, retry budget,
   current segment, current harness graph, graph sequence, harness graph
   stage, `continuation_goal`, and failure reason.
3. Scope planner, generator, verifier, and evaluator task ids to a
   `HarnessGraph`.
4. Add `ComplexTaskRequestHandler` as the only creator and closer of
   `ComplexTaskRequest` and `TaskSegment` records, and the spawner of one
   `TaskSegmentManager` per open segment.
5. Add `TaskSegmentManager` (per-segment) as the only creator of `HarnessGraph`
   records inside its owned segment, and the sole emitter of
   `SegmentCloseReport`.
6. Add repository/store helpers used by the lifecycle services for:
   - inserting a complex task request,
   - inserting the initial task segment,
   - inserting a continuation task segment,
   - inserting the next harness graph after a segment-manager retry decision,
   - loading the current segment for a request,
   - loading the current harness graph for a segment,
   - walking `requested_by_task_id`,
   - walking `previous_segment_id`,
   - listing harness graphs by segment and graph sequence order.
7. Backfill or compatibility-map existing graph-as-attempt state as needed.

## Phase exit criteria

- The runtime can create and load a `ComplexTaskRequest`.
- The runtime can create segment 1 with harness graph sequence 1.
- Tests cover `request_complex_task_solution` creating a request linked to
  `requested_by_task_id`.
- Tests cover continuation creating `TaskSegment` N+1 with
  `previous_segment_id` when the previous segment's `continuation_goal` is
  non-null.
- Tests prove `TaskSegmentManager` retry creates another `HarnessGraph` in the
  same segment, not a new segment or request.
