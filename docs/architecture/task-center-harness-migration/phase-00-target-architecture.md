# Phase 00 - Target Architecture

## Goal

Define the target harness model before changing implementation details.

The new architecture separates three different ideas that were previously
overloaded onto graph and retry state:

- A `ComplexTaskRequest` is the complex delegated goal requested by an
  executor that cannot solve its current task atomically.
- A `TaskSegment` is one vertical slice of that complex task. Segment 2+ exists
  only when the previous segment completed a partial plan.
- A `HarnessGraph` is one concrete planner-produced graph for one segment.
  Retry creates another `HarnessGraph` in the same segment.

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
  |     |     retry_no = 1
  |     |     status = failed
  |     |
  |     `-- HARNESS GRAPH S1.H2
  |           retry_no = 2
  |           status = passed
  |           plan_shape = partial
  |
  |     segment closes with plan_shape = partial
  |     because S1 is partial,
  |     ComplexTaskOrchestrator creates S2
  |
  +-- TASK SEGMENT S2
  |     sequence_no = 2
  |     previous_segment_id = S1
  |     created because S1 closed with plan_shape = partial
  |
  |     `-- HARNESS GRAPH S2.H1
  |           retry_no = 1
  |           status = passed
  |           plan_shape = full
  |
  |     segment closes with plan_shape = full
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
        retry_budget
        |
        `-- HarnessGraph
              retry_no
              stage
              status
              plan_shape
              fail_reason
              |
              +-- planner task
              +-- generator DAG tasks
              |     executors + verifiers
              `-- evaluator task
```

Explorer subagents are not TaskCenter nodes; they are non-blocking,
parallel-safe helper runs. Advisor and resolver helper calls are also not
TaskCenter graph nodes. Advisor is read-only. Resolver is blocking and may
edit, but it reports back into the task that called it.

## Three axes of progression

| Axis | Entity | What changes | Triggered by | Shape effect |
| ---- | ------ | ------------ | ------------ | ------------ |
| Request origin | `ComplexTaskRequest` | new delegated complex goal | `request_complex_task_solution(goal)` | new request chain owned by the calling executor |
| Vertical | `TaskSegment` | next partial-plan segment | successful partial plan | segment sequence increases |
| Horizontal | `HarnessGraph` | same segment, fresh try | graph failure under retry budget | retry number increases |

### Request origin

A `ComplexTaskRequest` represents the executor handoff:

- the executor task that requested help,
- the goal it requested,
- the eventual result that returns to that executor.

The requesting executor is the stable parent for context management.
`requested_by_task_id` is the authoritative origin and return link.

### Vertical axis

A `TaskSegment` represents one sequential continuation step in a complex task.
New segments are created only when a harness graph passes with
`plan_shape = partial`.

The segment chain is:

```
S1 -> S2 -> S3
```

`previous_segment_id` is only for partial-plan continuation. It is not a retry
chain.

### Horizontal axis

A `HarnessGraph` is one full `planner -> DAG -> evaluator` pass for one task
segment. When a harness graph fails and retry budget remains,
`ComplexTaskOrchestrator` creates the next `HarnessGraph` in the same
`TaskSegment`.

Retry never creates a new `ComplexTaskRequest` or `TaskSegment`.

## Why the split matters

- Complex-task context starts from the executor that requested the solution.
- Segment sequence reflects partial-plan continuation, not retry count.
- Retry history lives as harness graphs inside one task segment.
- Bubble-up to the requesting executor happens only when the complex task
  request closes.
- From the requesting executor's perspective, one request returns one final
  result.

## Components

| Component | Owner / scope | Responsibility |
| --------- | ------------- | -------------- |
| `ComplexTaskRequest` | `TaskCenter` | Container for a non-atomic delegated goal. Holds `requested_by_task_id`, goal, status, and final close result. |
| `TaskSegment` | `ComplexTaskRequest` | One vertical partial-plan segment. Holds sequence, previous segment, retry budget, and current harness graph. |
| `HarnessGraph` | `TaskSegment` | One concrete planner DAG execution: planner, generator DAG, evaluator, status, plan shape, and failure reason. |
| `ComplexTaskOrchestrator` | `TaskCenter` service / one active controller per `ComplexTaskRequest` | Owns the complex-task lifecycle. It is the only component that creates `ComplexTaskRequest`, `TaskSegment`, and `HarnessGraph` records; it enforces ids, sequence numbers, retry numbers, parent links, initial state, retry, partial continuation, and final return to the requesting executor. |
| `HarnessGraphOrchestrator` | one per `HarnessGraph` | Runs one planner-produced graph through planner, generator DAG tasks, and evaluator. It reports the graph outcome back to `ComplexTaskOrchestrator`. |
| Tasks | per `HarnessGraph` | Planner, executor, verifier, and evaluator agent runs scoped to one harness graph. |

## Complex Task Orchestrator

All structural creation goes through `ComplexTaskOrchestrator`.

`ComplexTaskOrchestrator` owns:

- `create_complex_task_request(requested_by_task_id, goal, context)`,
- `create_initial_segment(complex_task_request_id)`,
- `create_continuation_segment(previous_segment_id, continuation_goal)`,
- `create_initial_harness_graph(task_segment_id)`,
- `create_retry_harness_graph(task_segment_id, failed_harness_graph_id)`.

`HarnessGraphOrchestrator` decides the outcome of one harness graph.
`ComplexTaskOrchestrator` decides whether that outcome closes the segment,
creates a retry graph, creates a continuation segment, or closes the request.

## Phase exit criteria

- The team agrees that `ComplexTaskRequest` is the executor-requested complex
  goal.
- The team agrees that `TaskSegment` is only for partial-plan continuation.
- The team agrees that `HarnessGraph` is the retryable planner DAG execution.
- The team agrees that retry is horizontal and creates a new `HarnessGraph`
  inside the same `TaskSegment`.
- The team agrees that `ROOT` is not a creation or spawn reason.
- The context-engine boundary is explicit: planner launch context, per-graph
  evidence, detailed close-report payloads, and segment visibility are
  specified separately.
