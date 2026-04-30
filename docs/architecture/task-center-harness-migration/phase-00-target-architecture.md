# Phase 00 - Target Architecture

## Goal

Define the target harness model before changing implementation details.

The new architecture separates three different ideas that were previously
overloaded onto graph and retry state:

- A `ComplexTaskRequest` is the complex delegated goal requested by an
  executor that cannot solve its current task atomically.
- A `TaskSegment` is one vertical slice of that complex task. Segment 2+ exists
  only when the previous segment closed with a non-null `continuation_goal`.
- A `HarnessGraph` is one concrete planner-produced graph for one segment.
  It does not carry retry policy; `TaskSegmentManager` decides whether a
  failed graph should be followed by another graph in the same segment.

## Executor tool convention

Executor tools use two naming families:

| Prefix | Meaning |
| ------ | ------- |
| `submit_*` | Terminal outcome for the current executor task. |
| `request_*` | Non-terminal orchestration request that can pause and later resume the executor. |

Executor tool surface:

| Tool | Meaning |
| ---- | ------- |
| `submit_execution_success` | The executor completed and verified its assigned task. |
| `submit_execution_failure` | The executor has a scoped failure that cannot be completed directly. |
| `request_complex_task_solution` | The assigned task is not atomic; create a planned complex-task workflow and return the result to this executor. |

`request_complex_task_solution` is not a failure terminal. It is a handoff to
the complex-task harness.

## Target model

```text
USER QUERY
  |
  v
EXECUTOR TASK
  |
  +-- atomic task
  |     `-- submit_execution_success / submit_execution_failure
  |
  `-- non-atomic task
        `-- request_complex_task_solution(goal)
              |
              v
COMPLEX TASK REQUEST C1
  requested_by_task_id = executor
  goal = requested complex goal
  |
  +-- TASK SEGMENT S1
  |     sequence_no = 1
  |     previous_segment_id = null
  |
  |     +-- HARNESS GRAPH S1.H1
  |     |     graph_sequence_no = 1
  |     |     status = failed
  |     |
  |     `-- HARNESS GRAPH S1.H2
  |           graph_sequence_no = 2
  |           status = passed
  |           continuation_goal = "<next-segment goal>"
  |
  |     S1.H2 passes, so S1 closes
  |     S1.continuation_goal = S1.H2.continuation_goal
  |     because S1.continuation_goal is not null,
  |     TaskSegmentManager creates S2
  |
  +-- TASK SEGMENT S2
  |     sequence_no = 2
  |     previous_segment_id = S1
  |     goal = S1.continuation_goal
  |
  |     `-- HARNESS GRAPH S2.H1
  |           graph_sequence_no = 1
  |           status = passed
  |           continuation_goal = null
  |
  |     S2.H1 passes, so S2 closes with continuation_goal = null
  |
  `-- final complex-task result returns to requested_by_task_id
```

The core ownership shape is:

```text
ComplexTaskRequest
  requested_by_task_id
  goal
  status
  |
  `-- TaskSegment
        sequence_no
        previous_segment_id
        goal
        retry_budget
        continuation_goal
        |
        `-- HarnessGraph
              graph_sequence_no
              stage
              status
              continuation_goal
              fail_reason
              |
              +-- planner task
              +-- generator DAG tasks
              |     executors + verifiers
              `-- evaluator task
```

`continuation_goal` is set on a harness graph by that graph's own planner
submission: `null` from `submit_full_plan`, the supplied goal from
`submit_partial_plan(continuation_goal)`. Each harness graph's
`continuation_goal` is independent — a later graph in the same segment does
not inherit it from a prior failed graph; the new planner decides freshly.
When a harness graph passes, it closes the segment, and the segment inherits
`continuation_goal` from that passing graph (the last harness graph in the
segment, since failed graphs return to `TaskSegmentManager` for the retry
decision rather than closing).
`should_continue` is a derived predicate (`continuation_goal is not None`)
and is not stored.

Explorer subagents are not TaskCenter nodes; they are non-blocking,
parallel-safe helper runs. Advisor and resolver helper calls are also not
TaskCenter graph nodes. Advisor is read-only. Resolver is blocking and may
edit, but it reports back into the task that called it.

## Three axes of progression

| Axis | Entity | What changes | Triggered by | Shape effect |
| ---- | ------ | ------------ | ------------ | ------------ |
| Request origin | `ComplexTaskRequest` | new delegated complex goal | `request_complex_task_solution(goal)` | new request chain owned by the calling executor |
| Vertical | `TaskSegment` | next continuation segment | segment closed with `continuation_goal != null` | segment sequence increases |
| Horizontal | `HarnessGraph` | same segment, fresh graph execution | graph failure followed by a `TaskSegmentManager` retry decision | graph sequence increases |

### Request origin

A `ComplexTaskRequest` represents the executor handoff:

- the executor task that requested help,
- the goal it requested,
- the eventual result that returns to that executor.

The requesting executor is the stable parent for context management.
`requested_by_task_id` is the authoritative origin and return link.

### Vertical axis

A `TaskSegment` represents one sequential continuation step in a complex task.
A new segment is created only when the previous segment closed with a non-null
`continuation_goal`.

The segment chain is:

```
S1 -> S2 -> S3
```

`previous_segment_id` is the continuation lineage. It is not a retry chain.

### Segment close rule

A passing harness graph closes its segment. The segment inherits its
`continuation_goal` from that graph:

```text
TaskSegment S has running HarnessGraph H

H passes
  S.continuation_goal = H.continuation_goal
  S closes
    if S.continuation_goal is not None:
      ComplexTaskRequestHandler creates S+1 with goal = S.continuation_goal
    else:
      report request-level success

H fails
  if retry budget remains:
    TaskSegmentManager creates the next HarnessGraph in S
  else:
    close S failed
    report request-level failure
```

There is no policy hook for "spend retry on a passed graph": once a graph
passes, it closes the segment. Plan quality is enforced by the evaluator's
pass/fail decision, not by the segment manager.

### Recursive request boundary

Complex-task requests can be nested. Any generator executor inside any
`HarnessGraph` may call `request_complex_task_solution(goal)`. That creates a
new `ComplexTaskRequest`; it does not create a child `TaskSegment` in the outer
request.

```text
ComplexTaskRequest C1
  |
  `-- TaskSegment S1
        |
        `-- HarnessGraph S1.H1
              |
              `-- executor task E7
                    |
                    | request_complex_task_solution(goal)
                    v
              ComplexTaskRequest C2
                requested_by_task_id = E7

C2 closes
  |
  v
E7 resumes inside C1.S1.H1
```

The nested request has its own segment chain and retry history. The outer
request sees only the close report returned to the executor that requested it.

### Horizontal axis

A `HarnessGraph` is one full `planner -> DAG -> evaluator` pass for one task
segment. When a harness graph fails, `TaskSegmentManager` decides whether
segment retry budget should be spent. If it retries, it creates the next
`HarnessGraph` in the same `TaskSegment`.

Retry never creates a new `ComplexTaskRequest` or `TaskSegment`.

## Why the split matters

- Complex-task context starts from the executor that requested the solution.
- Segment sequence reflects continuation, not retry count.
- Retry history is derived from the ordered harness graphs inside one task
  segment; it is not encoded as harness-graph identity.
- Bubble-up to the requesting executor happens only when the complex task
  request closes.
- From the requesting executor's perspective, one request returns one final
  result.

## Components

| Component | Owner / scope | Responsibility |
| --------- | ------------- | -------------- |
| `ComplexTaskRequest` | `TaskCenter` | Container for a non-atomic delegated goal. Holds `requested_by_task_id`, goal, status, and final close result. |
| `TaskSegment` | `ComplexTaskRequest` | One vertical continuation segment. Holds sequence, previous segment, segment goal, segment retry budget, current harness graph, and `continuation_goal` inherited from the harness graph that closed it. |
| `HarnessGraph` | `TaskSegment` | One concrete planner DAG execution: graph sequence within the segment, planner, generator DAG, evaluator, status, `continuation_goal`, and failure reason. |
| `ComplexTaskRequestHandler` | request boundary / one active handler per `ComplexTaskRequest` | Owns the executor handoff from `request_complex_task_solution`, creates and closes the request, owns the segment chain (creates initial and continuation `TaskSegment` records, spawns a fresh `TaskSegmentManager` per segment), pauses/resumes the requesting executor, and returns the final report to `requested_by_task_id`. |
| `TaskSegmentManager` | one active manager per `TaskSegment` | Owns harness-graph transitions inside one segment: retry budget, next-graph creation after failed graphs, and segment close. Reports the segment close outcome (terminal success, success-with-continuation, or failed) back to `ComplexTaskRequestHandler`. Does not create the next segment. |
| `HarnessGraphOrchestrator` | one per `HarnessGraph` | Runs one planner-produced graph through planner, generator DAG tasks, and evaluator. It reports the graph outcome back to its `TaskSegmentManager`. |
| Tasks | per `HarnessGraph` | Planner, executor, verifier, and evaluator agent runs scoped to one harness graph. |

## Runtime Layers

The runtime uses three explicit layers:

| Layer | Owns | Does not own |
| ----- | ---- | ------------ |
| `ComplexTaskRequestHandler` | request creation, request close, **segment chain (initial and continuation segment creation)**, executor pause/resume, final close report | retry policy inside a segment or graph execution |
| `TaskSegmentManager` | one segment's retry budget, next harness graph creation after failed graphs, segment close decision | continuation segment creation, executor tool boundary, planner/generator/evaluator execution |
| `HarnessGraphOrchestrator` | one `planner -> generator DAG -> evaluator` execution | retry, continuation, or request close |

`ComplexTaskRequestHandler` owns:

- `create_complex_task_request(requested_by_task_id, goal, context)`,
- `create_initial_segment(complex_task_request_id)` — creates the segment record and spawns its `TaskSegmentManager`,
- `create_continuation_segment(previous_segment_id, continuation_goal)` — creates the next segment record and spawns a fresh `TaskSegmentManager` for it,
- `handle_segment_closed(segment_close_report)` — routes terminal success, continuation, or failure,
- `close_complex_task_request(complex_task_request_id, final_result)`.

`TaskSegmentManager` owns:

- `create_initial_harness_graph(task_segment_id)`,
- `create_next_harness_graph(task_segment_id, previous_harness_graph_id)`,
- `handle_harness_graph_closed(harness_graph_id)` — emits a `SegmentCloseReport` to `ComplexTaskRequestHandler` when the segment closes.

`create_next_harness_graph` follows a failed graph only after
`TaskSegmentManager` decides to spend segment retry budget. A passed graph
closes the segment; it never produces another graph.

The `SegmentCloseReport` is the only signal `TaskSegmentManager` sends to
`ComplexTaskRequestHandler`:

```
SegmentCloseReport {
  segment_id
  closing_harness_graph_id     # the passing graph, or last failed graph if exhausted
  outcome ∈ {
    success_terminal,            # passing graph; continuation_goal is null
    success_continue(goal),      # passing graph; continuation_goal is non-null
    failed_exhausted(reason),    # retry budget exhausted on failed graph
  }
}
```

`HarnessGraphOrchestrator` decides the outcome of one harness graph.
`TaskSegmentManager` decides whether that outcome retries inside the segment or
closes the segment. `ComplexTaskRequestHandler` decides whether the segment
close starts a continuation segment or closes the request, and resumes the
requesting executor on request close.

## Lifecycle Interaction Diagram

The lifecycle has three handoff boundaries:

```text
Executor task E
  |
  | request_complex_task_solution(goal)
  v
ComplexTaskRequestHandler
  |
  | create ComplexTaskRequest C
  |   requested_by_task_id = E
  |   status = open
  |
  | create TaskSegment S1
  |   sequence_no = 1
  |   previous_segment_id = null
  |   goal = C.goal
  | spawn TaskSegmentManager(S1)
  v
TaskSegmentManager(S1)
  |
  | create HarnessGraph S1.H1
  |   graph_sequence_no = 1
  v
HarnessGraphOrchestrator(S1.H1)
  |
  | run planner -> generator DAG -> evaluator
  |
  +-- graph failed ----------------------------+
  |                                            |
  v                                            |
TaskSegmentManager(S1)                        |
  |                                            |
  | retry budget remains                       |
  | create HarnessGraph S1.H2                  |
  v                                            |
HarnessGraphOrchestrator(S1.H2) <-------------+
  |
  | run planner -> generator DAG -> evaluator
  |
  +-- graph passed with continuation_goal != null
  v
TaskSegmentManager(S1)
  |
  | close TaskSegment S1
  |   S1.continuation_goal = S1.H2.continuation_goal
  | emit SegmentCloseReport { outcome = success_continue(goal) }
  v
ComplexTaskRequestHandler
  |
  | create TaskSegment S2
  |   sequence_no = 2
  |   previous_segment_id = S1
  |   goal = S1.continuation_goal
  | spawn TaskSegmentManager(S2)
  v
TaskSegmentManager(S2)
  |
  | create HarnessGraph S2.H1
  |   graph_sequence_no = 1
  v
HarnessGraphOrchestrator(S2.H1)
  |
  | run planner -> generator DAG -> evaluator
  |
  +-- graph passed with continuation_goal = null
  v
TaskSegmentManager(S2)
  |
  | close TaskSegment S2
  |   S2.continuation_goal = null
  | emit SegmentCloseReport { outcome = success_terminal }
  v
ComplexTaskRequestHandler
  |
  | close ComplexTaskRequest C
  | deliver complex_task_succeeded report
  v
Executor task E resumes
```

Failure follows the same boundary:

```text
HarnessGraphOrchestrator(H)
  |
  | graph failed
  v
TaskSegmentManager(S_n)
  |
  +-- retry budget remains
  |     create next HarnessGraph in the same TaskSegment
  |
  `-- retry exhausted
        close TaskSegment failed
        emit SegmentCloseReport { outcome = failed_exhausted(reason) }
        |
        v
ComplexTaskRequestHandler
        |
        close ComplexTaskRequest failed
        deliver complex_task_failed report to requested_by_task_id
```

## Phase exit criteria

- The team agrees that `ComplexTaskRequest` is the executor-requested complex
  goal.
- The team agrees that `TaskSegment` is the continuation unit and inherits its
  `continuation_goal` from the harness graph that closes it.
- The team agrees that `HarnessGraph` is a planner DAG execution ordered
  within a segment and carries the planner-emitted `continuation_goal`.
- The team agrees that retry is a `TaskSegmentManager` decision; when it
  retries, it creates another `HarnessGraph` inside the same `TaskSegment`.
- The team agrees that a passed harness graph always closes its segment;
  there is no `retry_after_partial` and no separate "accept" step.
- The team agrees that `ROOT` is not a creation or spawn reason.
- The context-engine boundary is explicit: planner launch context, per-graph
  evidence, detailed close-report payloads, and segment visibility are
  specified separately.
