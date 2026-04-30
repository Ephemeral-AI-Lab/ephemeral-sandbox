# Phase 04 - Complex Task Spawning and Handoff

## Goal

Implement complex-task request creation after the durable model, orchestrators,
and tool-gate foundations are in place.

The target model has no partial-plan continuation. A `ComplexTaskRequest`
creates exactly one `TaskSegment`; segment-manager retry creates new
`HarnessGraph`s inside that segment.

`ComplexTaskRequestHandler` is the only creator of `ComplexTaskRequest` and
`TaskSegment` records, and the only spawner of `TaskSegmentManager` instances.
Each per-segment `TaskSegmentManager` is the only creator of `HarnessGraph`
records inside its owned segment.

## Creation path

```text
executor task E
  |
  +-- request_complex_task_solution(goal)
        ComplexTaskRequestHandler creates ComplexTaskRequest C
          C.requested_by_task_id = E
        ComplexTaskRequestHandler creates TaskSegment S1
          S1.goal = C.goal
        ComplexTaskRequestHandler spawns TaskSegmentManager(S1)
        TaskSegmentManager(S1) creates HarnessGraph H1
```

`request_complex_task_solution` starts a new complex-task request. It does not
create another segment in an existing request.

## Field mapping

| Creation path | Entity created | Created by | Parent / lineage |
| ------------- | -------------- | ---------- | ---------------- |
| `request_complex_task_solution` | `ComplexTaskRequest` | `ComplexTaskRequestHandler` | `requested_by_task_id` is the executor that called the tool |
| initial segment | `TaskSegment` | `ComplexTaskRequestHandler` | `complex_task_request_id = C`, `sequence_no = 1`, `goal = C.goal` |
| initial graph | `HarnessGraph` | `TaskSegmentManager(S)` | `task_segment_id = S`, `graph_sequence_no = 1` |
| subsequent graph after failed graph | `HarnessGraph` | `TaskSegmentManager(S)` | same `task_segment_id`, `graph_sequence_no = previous + 1`; created only after the manager decides to spend retry budget |

There is no `ROOT` spawn reason. Retry is not `TaskSegment` creation and is not
a `HarnessGraph` creation reason.

## `request_complex_task_solution` workflow

```text
Executor task E is running inside some harness graph

E calls request_complex_task_solution(goal)
    |
    v
ComplexTaskRequestHandler creates ComplexTaskRequest C
  requested_by_task_id = E
  goal                 = goal
    |
    v
ComplexTaskRequestHandler creates TaskSegment S1 and spawns TaskSegmentManager(S1)
    |
    v
TaskSegmentManager(S1) creates HarnessGraph S1.H1
    |
    v
HarnessGraphOrchestrator runs S1.H1 to completion
    |
    v
TaskSegmentManager(S1) retries inside S1, or closes S1 and emits TaskSegmentClosureReport
    |
    v
ComplexTaskRequestHandler closes C with success or failure
    |
    v
ComplexTaskRequestHandler delivers complex_task_succeeded or
complex_task_failed report to executor task E
    |
    v
The outer graph consumes E's final task result
```

`request_complex_task_solution` may happen at any graph depth and during any
generator executor task. The call is a handoff: the original executor agent run
ends at the request boundary and does not submit a second terminal.

## Recursive complex-task requests

Complex-task requests are recursive. Any generator executor running inside a
`HarnessGraph` can call `request_complex_task_solution(goal)` before it edits.
That call creates a new `ComplexTaskRequest` whose `requested_by_task_id` is the
executor task that called the tool.

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
                |
                `-- TaskSegment S1
                      |
                      `-- HarnessGraph S1.H1

C2 closes
  |
  v
ComplexTaskRequestHandler returns C2 close report as E7's final task result
```

The nested request does not become a child `TaskSegment` of the outer request.
The nested request has its own single segment and retry history.

## Close reports

A `HarnessGraph` closes exactly once. Its outcome feeds the owning segment.
A `TaskSegment` closes exactly once. Its close report causes
`ComplexTaskRequestHandler` to close the request successfully or as failed.

The complex-task close report returned to `requested_by_task_id` has these
harness-owned fields:

| Field | Meaning |
| ----- | ------- |
| `complex_task_request_id` | request id |
| `requested_by_task_id` | executor task that requested the complex solution |
| `outcome` | `success` or `failed` |
| `final_segment_id` | segment that produced the final outcome |
| `final_harness_graph_id` | harness graph that produced the final outcome |

Detailed payload such as per-task summaries, planner scratchpads, and evidence
links belongs to the context engine.

## Close-report routing

| Event | Routing |
| ----- | ------- |
| `ComplexTaskRequest` closes | final report is attached to the executor task that called `request_complex_task_solution` |
| `TaskSegment` closes succeeded | `TaskSegmentManager` emits `terminal_success`; `ComplexTaskRequestHandler` closes the complex task request successfully |
| `TaskSegment` closes failed | `TaskSegmentManager` emits `attempt_plan_failed(attempted_plan_history)`; `ComplexTaskRequestHandler` closes the complex task request as failed |

Retry never returns a close report to the requesting executor. Retry is internal
motion inside one task segment.

## Implementation tasks

1. Implement `request_complex_task_solution` creation of `ComplexTaskRequest`
   through `ComplexTaskRequestHandler`.
2. Treat `request_complex_task_solution` as a handoff whose final result is
   supplied by the complex-task close report.
3. Create initial `TaskSegment` through `ComplexTaskRequestHandler`, spawn
   `TaskSegmentManager(S1)`, then have the manager create the initial
   `HarnessGraph`.
4. Keep exactly one `TaskSegment` per `ComplexTaskRequest`.
5. Route final complex-task close reports back to the requesting executor task.
6. Add close-report persistence or delivery semantics robust enough for process
   restart if the surrounding runtime supports it.

## Phase exit criteria

- `request_complex_task_solution` creates a complex task request and its final
  close report becomes the requesting executor task result.
- Each complex task request creates exactly one task segment.
- Retry stays inside the same segment and does not produce executor close reports
  until the complex task request closes.
- Partial-plan continuation is absent from the request and segment lifecycle.
