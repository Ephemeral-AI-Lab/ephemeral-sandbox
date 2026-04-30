# Phase 01 - Complex Task Request and Harness Graph Model

## Goal

Introduce the durable state model required by the new harness shape before
`ComplexTaskRequestHandler`, `TaskSegmentManager`, and
`HarnessGraphOrchestrator` behavior is migrated.

This phase is mostly schema, persistence, and typed runtime state. It should not
change high-level execution behavior until Phase 02 starts using the new model.

Each `ComplexTaskRequest` tracks its owned `TaskSegment` records with an
ordered `task_segment_ids` list. Phase 01 only needs to create the initial
segment, but the request shape must allow more than one segment. Retry creates
additional `HarnessGraph` records inside the current segment.

## Durable entities

### `ComplexTaskRequest`

A `ComplexTaskRequest` is a complex delegated goal requested by an executor that
decided its assigned task is not atomic.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `id` | Complex task request id. |
| `task_center_run_id` | Owning TaskCenter run. Do not use the generic `run_id` name here; agent execution records have their own run ids. |
| `requested_by_task_id` | Executor task that called `request_complex_task_solution`. |
| `goal` | Goal supplied by the executor. |
| `status` | `open`, `succeeded`, `failed`, or `cancelled`. |
| `task_segment_ids` | Ordered list of `TaskSegment` ids owned by this request. Starts with the initial segment and may contain multiple segments. |
| `created_at` / `updated_at` / `closed_at` | Lifecycle timestamps. |

`requested_by_task_id` is the authoritative parent link for context and final
result routing.

### `TaskSegment`

A `TaskSegment` is one request-local execution scope for a complex task request.
The initial segment starts from the requested goal; later segments use the goal
assigned when they are appended. Each segment owns attempt budget for harness
graph attempts.

Required fields:

| Field | Meaning |
| ----- | ------- |
| `id` | Segment id. |
| `complex_task_request_id` | Owning complex task request. |
| `sequence_no` | 1-based segment order within the request. |
| `creation_reason` | Segment creation reason; `initial` for the first segment. |
| `goal` | Segment goal. Segment 1 equals the request goal. |
| `attempt_budget` | Maximum harness graph attempts for this segment. |
| `status` | `open`, `succeeded`, `failed`, or `cancelled`. |
| `harness_graph_ids` | Ordered list of `HarnessGraph` ids owned by this segment. Attempts can be inferred from this list. |

Segment ordering is recorded by `ComplexTaskRequest.task_segment_ids` and
`TaskSegment.sequence_no`; there is no `previous_segment_id` lineage.

### `HarnessGraph`

A `HarnessGraph` is one full planner-produced graph execution for one segment.
It runs `planner -> generator DAG -> evaluator`. Retry policy is not stored on
the graph; `TaskSegmentManager` decides whether a failed graph should be
followed by another graph in the same segment.

```text
HarnessGraph {
    segment_id:          owning TaskSegment
    graph_sequence_no:   1-based graph order inside the segment
    stage:               planning | generating | evaluating | closed
    planner_task_id:     uuid
    task_specification:  string from submit_full_plan
    evaluation_criteria: [criterion, ...]
    generator_task_ids:  [executor_1, verifier, ...]
    evaluator_task_id:   null | uuid
    status:              running | passed | failed
    fail_reason:         null
                       | planner_step_budget_exhausted
                       | generator_failed
                       | evaluator_failed
}
```

Per-harness-graph evidence such as task summaries, planner scratchpads, and
artifact references belongs to the context engine. The harness model stores only
the structural state needed for lifecycle decisions.

`task_specification` and `evaluation_criteria` are the segment contract emitted
by the planner. `HarnessGraphOrchestrator` passes them to the evaluator as
evaluation instructions. The harness graph that passes closes its segment, and
its contract is the segment's accepted record.

Generator ordering and dependency constraints live on task records rather than
on `HarnessGraph`.

`evaluator_task_id` is unset while the graph is in `planning` or `generating`.
`HarnessGraphOrchestrator` creates the evaluator only after every generator task
in the current graph has completed successfully.

There is no `ROOT` spawn or creation reason.

## Creation reasons and lineage

| Entity | Creation reason | Trigger | Parent / lineage |
| ------ | --------------- | ------- | ---------------- |
| `ComplexTaskRequest` | implicit complex-task request | Executor calls `request_complex_task_solution(goal)` | `requested_by_task_id` points to the executor. |
| `TaskSegment` | `initial` for the first segment | Complex task request starts, or a later request-level flow appends another segment | `complex_task_request_id` points to the request; the segment id is appended to `task_segment_ids`. |
| `HarnessGraph` | none | Segment manager starts a graph execution | `segment_id` points to the owned segment; `graph_sequence_no = 1` for the first graph, or previous + 1 for later graphs. |

Retry is never a `ComplexTaskRequest` or `TaskSegment` creation reason. It is a
`TaskSegmentManager` decision after a failed graph within the current segment.
A passing harness graph closes its segment; it never produces another graph.

## Context walks

Three context walks coexist:

- Request origin: `ComplexTaskRequest.requested_by_task_id`.
- Request segment order: `ComplexTaskRequest.task_segment_ids` plus
  `TaskSegment.sequence_no`.
- Horizontal graph history: `HarnessGraph.segment_id` plus lower
  `graph_sequence_no` values. Retry context is derived from prior failed graphs
  by `TaskSegmentManager` and the context engine.

The context engine can compose these into:

```text
ComplexTaskRequest
  goal = goal from requesting executor
  task_segment_ids = [S1, ...]
  |
  +-- TaskSegment 1
  |     |
  |     +-- HarnessGraph 1
  |     |     initial try, failed
  |     |
  |     `-- HarnessGraph 2
  |           second graph after failed graph, passed
  |           segment closes
  |
  `-- TaskSegment N
        |
        `-- HarnessGraph 1
              later segment try, passed
              complex task request reports back to requested_by_task_id
```

## Segment retry policy

`TaskSegment.attempt_budget` is set at segment creation. It may come from a
runtime default or request-level configuration, but it is applied segment-locally.

`TaskSegment.harness_graph_ids` is the ordered source of truth for harness graph
attempts within a segment. Expose a public `get_attempt_count(task_segment)`
helper that returns the count derived from `harness_graph_ids` rather than
storing a separate counter.

## Lifecycle Services

Add three lifecycle services. Runtime tool handlers and
`HarnessGraphOrchestrator`s should not manually assemble
`ComplexTaskRequest`, `TaskSegment`, or `HarnessGraph` records.

`ComplexTaskRequestHandler` owns the request boundary and attaches request
segments. It is the only creator of `ComplexTaskRequest` and `TaskSegment`
records:

| Method | Responsibility |
| ------ | -------------- |
| `create_complex_task_request(...)` | Create the request from `request_complex_task_solution`, set `requested_by_task_id`, store the goal, and initialize request status. |
| `create_initial_segment(...)` | Create segment 1 with `goal = request.goal`, set attempt budget, append it to `request.task_segment_ids`, and spawn a `TaskSegmentManager` bound to that segment. |
| `create_task_segment(...)` | Create a later segment with the next `sequence_no`, append it to `request.task_segment_ids`, and spawn a `TaskSegmentManager` bound to that segment. |
| `handle_segment_closed(...)` | Receive the `TaskSegmentClosureReport` from the per-segment `TaskSegmentManager`; route `terminal_success` and `attempt_plan_failed` to request close. |
| `close_complex_task_request(...)` | Store the final result and attach the complex-task close report to `requested_by_task_id`. |

`TaskSegmentManager` is per-`TaskSegment` and owns harness-graph transitions
inside that one segment. It is the only creator of `HarnessGraph` records:

| Method | Responsibility |
| ------ | -------------- |
| `create_initial_harness_graph(...)` | Create graph sequence 1 for the owned segment and append it to `harness_graph_ids`. |
| `create_next_harness_graph(...)` | Create graph sequence N+1 in the same segment after a failed harness graph and segment attempt-budget check. |
| `handle_harness_graph_closed(...)` | React to a graph outcome by either retrying inside the segment or closing the segment and emitting a `TaskSegmentClosureReport` to `ComplexTaskRequestHandler`. |
| `get_attempt_count(task_segment)` | Public helper that returns the number of harness graph attempts from `harness_graph_ids`. |

`TaskSegmentClosureReport` is the only signal from `TaskSegmentManager` to
`ComplexTaskRequestHandler`:

```text
TaskSegmentClosureReport {
  task_segment_id
  final_harness_graph_id     # passing graph, or final attempted failed graph
  outcome in {
    terminal_success,        # passing graph
    attempt_plan_failed {
      failure_summary,
      attempted_plan_history: [
        {
          harness_graph_summary_id,
          harness_graph_id,
          graph_sequence_no,
          task_specification,
          evaluation_criteria,
          fail_reason,
          failure_landscape,
        }
      ]
    },
  }
}
```

`attempt_plan_failed` contains the ordered plan attempts for the segment. Each
entry is derived from a closed harness graph summary and records both the plan
that was tried and the failure reason or failure landscape for that graph.

`TaskSegmentManager` must enforce these invariants:

- Subsequent harness graphs stay in the same segment.
- Graph sequence numbers are contiguous within a segment.
- A passing harness graph always closes the owned segment; it never produces a
  subsequent graph.
- A failed harness graph returns to `TaskSegmentManager`; the manager retries
  while attempt budget remains, and closes the segment failed once budget is exhausted.
- The segment is initialized with exactly one initial harness graph and closes
  exactly once.
- The manager never creates `ComplexTaskRequest` or `TaskSegment` records.

`ComplexTaskRequestHandler` must enforce these invariants:

- Every complex task request has one or more ordered `TaskSegment` ids in
  `task_segment_ids`.
- `task_segment_ids` contains each segment owned by the request exactly once.
- Exactly one `TaskSegmentManager` instance is active per open segment.
- A request, segment, or graph is initialized in exactly one valid opening
  state.

## Implementation tasks

1. Add or adapt typed models for `ComplexTaskRequest`, `TaskSegment`, and
   `HarnessGraph`.
2. Add persistence fields for request origin, ordered `task_segment_ids`, attempt
   budget, ordered `harness_graph_ids`, graph sequence, harness graph stage, and
   failure reason.
3. Scope planner, generator, verifier, and evaluator task ids to a
   `HarnessGraph`.
4. Add `ComplexTaskRequestHandler` as the only creator and closer of
   `ComplexTaskRequest` records, the only creator of `TaskSegment` records for a
   request, and the spawner of one `TaskSegmentManager` per created segment.
5. Add `TaskSegmentManager` as the only creator of `HarnessGraph` records inside
   its owned segment, and the sole emitter of `TaskSegmentClosureReport`.
6. Add repository/store helpers used by the lifecycle services for:
   - inserting a complex task request,
   - inserting the initial task segment,
   - inserting the next harness graph after a segment-manager retry decision,
   - loading ordered segments for a request,
   - loading the current segment for a request,
   - loading the current harness graph for a segment from the last
     `harness_graph_ids` entry,
   - walking `requested_by_task_id`,
   - listing harness graphs by segment and graph sequence order.
7. Backfill or compatibility-map existing graph-as-attempt state as needed.

## Phase exit criteria

- The runtime can create and load a `ComplexTaskRequest`.
- The runtime can create segment 1 with harness graph sequence 1.
- Tests cover `request_complex_task_solution` creating a request linked to
  `requested_by_task_id`.
- Tests prove each request records created segments in `task_segment_ids`.
- Tests prove `task_segment_ids` can hold multiple `TaskSegment` ids for one
  request.
- Tests prove `TaskSegmentManager` retry creates another `HarnessGraph` in the
  same segment, not a new segment or request.
