# Phase 00 - Target Architecture

## Goal

Define the target harness model before changing implementation details.

The new architecture separates three different ideas that were previously
overloaded onto graph and retry state:

- A `ComplexTaskRequest` is the complex delegated goal requested by an executor
  that cannot solve its current task atomically.
- A `TaskSegment` is the request-local execution segment. The target model has
  exactly one segment per complex task request.
- A `HarnessGraph` is one concrete planner-produced graph for that segment.
  It does not carry retry policy; `TaskSegmentManager` decides whether a failed
  graph should be followed by another graph in the same segment.

The partial-plan continuation feature is removed. Planners submit full plans
only, and retry is the only way a complex task request gets another
`HarnessGraph`.

## Executor tool convention

Executor tools use two naming families:

| Prefix      | Meaning                                                                     |
| ----------- | --------------------------------------------------------------------------- |
| `submit_*`  | Terminal outcome for the current executor task.                             |
| `request_*` | Orchestration handoff that delegates the executor task to another workflow. |

Executor tool surface:

| Tool                            | Meaning                                                                                                                         |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `submit_execution_success`      | The executor completed and verified its assigned task.                                                                          |
| `submit_execution_failure`      | The executor has a scoped failure that cannot be completed directly.                                                            |
| `request_complex_task_solution` | The assigned task is not atomic; create a planned complex-task workflow whose close report becomes this executor task's result. |

`request_complex_task_solution` is not a failure terminal. It exits the current
executor agent run by handing the task to the complex-task harness. The harness
later attaches a final close report to `requested_by_task_id`; the original
executor agent run ends at the handoff.

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
  `-- TASK SEGMENT S1
        sequence_no = 1
        |
        +-- HARNESS GRAPH S1.H1
        |     graph_sequence_no = 1
        |     status = failed
        |
        `-- HARNESS GRAPH S1.H2
              graph_sequence_no = 2
              status = passed

        S1.H2 passes, so S1 closes successfully
        C1 closes and reports back to requested_by_task_id
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
        goal
        retry_budget
        current_harness_graph_id
        |
        `-- HarnessGraph
              graph_sequence_no
              stage
              status
              fail_reason
              |
              +-- planner task
              +-- generator DAG tasks
              |     executors + verifiers
              `-- evaluator task
```

The planner emits exactly one full graph contract through `submit_full_plan`.
That contract is stored on `HarnessGraph.task_specification` and
`HarnessGraph.evaluation_criteria`. When a graph passes, the segment closes
successfully. When a graph fails, it returns to `TaskSegmentManager` for a
retry-budget decision.

Explorer subagents are not TaskCenter nodes; they are non-blocking,
parallel-safe helper runs. Advisor and resolver helper calls are also not
TaskCenter graph nodes. Advisor is read-only. Resolver is blocking and may edit,
but it reports back into the task that called it.

## Two axes of progression

| Axis             | Entity               | What changes                        | Triggered by                                                    | Shape effect                               |
| ---------------- | -------------------- | ----------------------------------- | --------------------------------------------------------------- | ------------------------------------------ |
| Request origin   | `ComplexTaskRequest` | new delegated complex goal          | `request_complex_task_solution(goal)`                           | new request linked to the calling executor |
| Horizontal retry | `HarnessGraph`       | same segment, fresh graph execution | graph failure followed by a `TaskSegmentManager` retry decision | graph sequence increases                   |

### Request origin

A `ComplexTaskRequest` represents the executor handoff:

- the executor task that requested help,
- the goal it requested,
- the eventual result attached back to that executor task.

The requesting executor is the stable parent for context management.
`requested_by_task_id` is the authoritative origin and report-delivery link.

### Segment boundary

A `TaskSegment` represents the complex task request's single execution scope.
It carries the segment-local retry budget and owns the ordered
`HarnessGraph` attempts.

There is no segment chain:

```text
ComplexTaskRequest C
  `-- TaskSegment S1
        +-- HarnessGraph H1
        `-- HarnessGraph H2
```

### Segment close rule

A passing harness graph closes its segment. A failed harness graph either
creates another graph inside the same segment or exhausts the segment:

```text
TaskSegment S has running HarnessGraph H

H passes
  S closes succeeded
  ComplexTaskRequest closes succeeded

H fails
  if retry budget remains:
    TaskSegmentManager creates the next HarnessGraph in S
  else:
    S closes failed
    ComplexTaskRequest closes failed
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
E7 receives the C2 close report as its final task result inside C1.S1.H1
```

The nested request has its own segment and retry history. The outer request sees
only the close report returned to the executor that requested it.

### Horizontal axis

A `HarnessGraph` is one full `planner -> DAG -> evaluator` pass for one task
segment. When a harness graph fails, `TaskSegmentManager` decides whether
segment retry budget should be spent. If it retries, it creates the next
`HarnessGraph` in the same `TaskSegment`.

Retry never creates a new `ComplexTaskRequest` or `TaskSegment`.

## Why the split matters

- Complex-task context starts from the executor that requested the solution.
- Segment state reflects the request-local retry scope, not a continuation
  chain.
- Retry history is derived from the ordered harness graphs inside one task
  segment; it is not encoded as harness-graph identity.
- From the requesting executor task's perspective, one request produces one
  final result.

## Components

| Component                   | Owner / scope                                                  | Responsibility                                                                                                                                                                                                             |
| --------------------------- | -------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ComplexTaskRequest`        | `TaskCenter`                                                   | Container for a non-atomic delegated goal. Holds `requested_by_task_id`, goal, status, and final close result.                                                                                                             |
| `TaskSegment`               | `ComplexTaskRequest`                                           | The request-local execution segment. Holds sequence, segment goal, retry budget, and current harness graph.                                                                                                                |
| `HarnessGraph`              | `TaskSegment`                                                  | One concrete planner DAG execution: graph sequence within the segment, planner, generator DAG, evaluator, status, and failure reason.                                                                                      |
| `ComplexTaskRequestHandler` | request boundary / one active handler per `ComplexTaskRequest` | Owns the executor handoff from `request_complex_task_solution`, creates and closes the request, creates the single `TaskSegment`, spawns its `TaskSegmentManager`, and returns the final report to `requested_by_task_id`. |
| `TaskSegmentManager`        | one active manager per `TaskSegment`                           | Owns harness-graph transitions inside one segment: retry budget, next-graph creation after failed graphs, and segment close. Reports the segment close outcome back to `ComplexTaskRequestHandler`.                        |
| `HarnessGraphOrchestrator`  | one per `HarnessGraph`                                         | Runs one planner-produced graph through planner, generator DAG tasks, and evaluator. It reports the graph outcome back to its `TaskSegmentManager`.                                                                        |
| Tasks                       | per `HarnessGraph`                                             | Planner, executor, verifier, and evaluator agent runs scoped to one harness graph.                                                                                                                                         |

## Runtime Layers

The runtime uses three explicit layers:

| Layer                       | Owns                                                                                                | Does not own                                                  |
| --------------------------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
| `ComplexTaskRequestHandler` | request creation, request close, single segment creation, final close report                        | retry policy inside a segment or graph execution              |
| `TaskSegmentManager`        | one segment's retry budget, next harness graph creation after failed graphs, segment close decision | executor tool boundary, planner/generator/evaluator execution |
| `HarnessGraphOrchestrator`  | one `planner -> generator DAG -> evaluator` execution                                               | retry or request close                                        |

`ComplexTaskRequestHandler` owns:

- `create_complex_task_request(requested_by_task_id, goal, context)`,
- `create_initial_segment(complex_task_request_id)` -- creates the segment record
  and spawns its `TaskSegmentManager`,
- `handle_segment_closed(segment_close_report)` -- routes success or failure to
  request close,
- `close_complex_task_request(complex_task_request_id, final_result)`.

`TaskSegmentManager` owns:

- `create_initial_harness_graph(task_segment_id)`,
- `create_next_harness_graph(task_segment_id, previous_harness_graph_id)`,
- `handle_harness_graph_closed(harness_graph_id)` -- emits a
  `TaskSegmentClosureReport` to `ComplexTaskRequestHandler` when the segment closes.

`create_next_harness_graph` follows a failed graph only after
`TaskSegmentManager` decides to spend segment retry budget. A passed graph
closes the segment; it never produces another graph.

The `TaskSegmentClosureReport` is the only signal `TaskSegmentManager` sends to
`ComplexTaskRequestHandler`:

```text
TaskSegmentClosureReport {
  task_segment_id
  final_harness_graph_id     # the passing graph, or final attempted failed graph
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

`attempt_plan_failed` is a segment outcome, not a retry-budget label. It means
every planned harness graph attempt for this segment has been tried and all
attempts failed. Its payload is derived from ordered harness graph summaries so
the requester can see what plans were attempted and why each one failed.

`HarnessGraphOrchestrator` decides the outcome of one harness graph.
c`TaskSegmentManager` decides whether that outcome retries inside the segment or
closes the segment. `ComplexTaskRequestHandler` decides request close and final
report delivery.

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
  +-- graph passed
  v
TaskSegmentManager(S1)
  |
  | close TaskSegment S1
  | emit TaskSegmentClosureReport { outcome = terminal_success }
  v
ComplexTaskRequestHandler
  |
  | close ComplexTaskRequest C
  | deliver complex_task_succeeded report
  v
Executor task E has final complex-task result
```

Failure follows the same boundary:

```text
HarnessGraphOrchestrator(H)
  |
  | graph failed
  v
TaskSegmentManager(S1)
  |
  +-- retry budget remains
  |     create next HarnessGraph in the same TaskSegment
  |
  `-- retry exhausted
        close TaskSegment failed
        emit TaskSegmentClosureReport { outcome = attempt_plan_failed(attempted_plan_history) }
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
- The team agrees that `TaskSegment` is the single request-local retry scope.
- The team agrees that `HarnessGraph` is a planner DAG execution ordered within
  the segment.
- The team agrees that retry is a `TaskSegmentManager` decision; when it retries,
  it creates another `HarnessGraph` inside the same `TaskSegment`.
- The team agrees that a passed harness graph always closes its segment.
- The team agrees that partial-plan continuation is removed.
- The team agrees that `ROOT` is not a creation or spawn reason.
- The context-engine boundary is explicit: planner launch context, per-graph
  evidence, detailed close-report payloads, and segment visibility are specified
  separately.
