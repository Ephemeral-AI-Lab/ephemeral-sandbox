# Phase 04 - Complex Task Spawning

## Goal

Implement complex-task request creation after the durable model, orchestrators,
and tool-gate foundations are in place.

The target model supports partial-plan continuation. A `ComplexTaskRequest`
starts with one `TaskSegment`, and `ComplexTaskRequestHandler` creates later
segments only when the previous segment closes with a non-null
`continuation_goal`. Segment-manager retry creates new `HarnessGraph`s inside
the current segment.

`ComplexTaskRequestHandler` is the only creator of `ComplexTaskRequest` and
`TaskSegment` records, and the only spawner of `TaskSegmentManager` instances.
Each per-segment `TaskSegmentManager` is the only creator of `HarnessGraph`
records inside its owned segment.

## Phase 01 inheritance

Phase 01 ships request creation, segment-chain construction, and close-report
assembly; Phase 04 wires the actual delivery to `requested_by_task_id`.

**Already in place:**

- `ComplexTaskRequestHandler.create_complex_task_request(task_center_run_id,
  requested_by_task_id, goal)` creates the request with
  `requested_by_task_id` recorded. `create_initial_segment` /
  `create_continuation_segment` enforce sequence-number contiguity and the
  predecessor SUCCEEDED + non-null `continuation_goal` precondition for
  continuation.
- `ComplexTaskCloseReport` DTO carries `complex_task_request_id`,
  `requested_by_task_id`, `outcome` (`"success"` | `"failed"`),
  `final_segment_id`, and `final_harness_graph_id`. It is constructed by
  `ComplexTaskRequestHandler._build_close_report` whenever a request closes.
- `ComplexTaskRequestHandler.close_complex_task_request(...)` invokes a
  `deliver_close_report: Callable[[ComplexTaskCloseReport], None] | None`
  callback if one is supplied (the parameter exists; it defaults to
  `None` in Phase 01). Verified end-to-end by
  `test_close_complex_task_request_delivers_close_report_when_callback_set`.
- `ComplexTaskRequestRecord.final_outcome` is persisted as a JSON dict
  shaped `{"outcome": "success" | "failed", "final_segment_id": ...,
  "final_harness_graph_id": ...}`.
- `/api/db/task-center-runs/{id}/graph` router currently returns
  `{"harness_graphs": []}` with a `# TODO(phase-04)` comment, so callers
  see the route shape but no data while the new walk is built.
- The integration smoke (`test_integration_smoke.py`) drives every
  segment-closure → request-closure path through stub orchestrators, so
  `ComplexTaskRequestStatus.SUCCEEDED` and `ComplexTaskRequestStatus.FAILED`
  transitions are already locked.

**Phase 04 wires:**

- A `deliver_close_report` callable that attaches the report to the
  executor task identified by `requested_by_task_id` and unblocks its
  outer agent run.
- The `request_complex_task_solution` tool handler delegates an accepted call
  to `ComplexTaskHandoffCoordinator`, which calls
  `ComplexTaskRequestHandler.create_complex_task_request` followed by
  `create_initial_segment`, then exits the executor agent run pending the
  close report (the Phase 03 tool gate guards the same entry point).
- Persistence or replay semantics for the close report so a process
  restart can still deliver to `requested_by_task_id`.
- The `/api/db/task-center-runs/{id}/graph` router endpoint, walking
  `complex_task_requests → task_segments → harness_graphs` to surface
  the new schema's harness-graph view to the frontend.

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
| continuation segment | `TaskSegment` | `ComplexTaskRequestHandler` | `complex_task_request_id = C`, `sequence_no = previous + 1`, `goal = previous_segment.continuation_goal`; the segment id is appended to `task_segment_ids` |
| initial graph | `HarnessGraph` | `TaskSegmentManager(S)` | `task_segment_id = S`, `graph_sequence_no = 1` |
| subsequent graph after failed graph | `HarnessGraph` | `TaskSegmentManager(S)` | same `task_segment_id`, `graph_sequence_no = previous + 1`; created only after the manager decides to spend attempt budget |

There is no `ROOT` spawn reason. Retry is not `TaskSegment` creation and is not
a `HarnessGraph` creation reason.

## `request_complex_task_solution` workflow

```text
Executor task E is running inside some harness graph

E calls request_complex_task_solution(goal)
    |
    v
ComplexTaskHandoffCoordinator starts delegated request handoff
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
    +-- success_continue(goal)
    |     ComplexTaskRequestHandler creates continuation TaskSegment S2
    |     and a fresh TaskSegmentManager(S2)
    |     TaskSegmentManager(S2) creates and starts HarnessGraph S2.H1
    |
    v
eventually:
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
generator executor task. The call starts a delegated complex-task request: the
original executor agent run ends at the request boundary and does not submit a
second terminal.

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
                +-- TaskSegment S1
                      |
                      `-- HarnessGraph S1.H1
                `-- TaskSegment S2
                      |
                      `-- HarnessGraph S2.H1

C2 closes
  |
  v
ComplexTaskRequestHandler returns C2 close report as E7's final task result
```

The delegated request does not become a child `TaskSegment` of the outer request.
The delegated request has its own segment chain and retry history.

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
| `TaskSegment` closes with continuation | `TaskSegmentManager` emits `success_continue(goal)`; `ComplexTaskRequestHandler` creates the next segment and keeps the request open |
| `TaskSegment` closes failed | `TaskSegmentManager` emits `attempt_plan_failed(attempted_plan_history)`; `ComplexTaskRequestHandler` closes the complex task request as failed |

Retry never returns a close report to the requesting executor. Retry is internal
motion inside one task segment.
Continuation also does not return to the requesting executor. It keeps the same
complex request open and creates the next segment.

## Implementation tasks

1. Implement `request_complex_task_solution` as a thin tool handler that
   delegates complex-request creation to `ComplexTaskHandoffCoordinator`.
2. Treat `request_complex_task_solution` as a delegated request start whose
   final result is supplied by the complex-task close report.
3. Create initial `TaskSegment` through `ComplexTaskRequestHandler`, spawn
   `TaskSegmentManager(S1)`, then have the manager create the initial
   `HarnessGraph`.
4. Implement continuation segment creation when a segment closes with
   `success_continue(goal)`, and ensure the fresh `TaskSegmentManager` creates
   and starts the continuation segment's initial `HarnessGraph`.
5. Route final complex-task close reports back to the requesting executor task.
6. Add close-report persistence or delivery semantics robust enough for process
   restart if the surrounding runtime supports it.

## Phase exit criteria

- `request_complex_task_solution` creates a complex task request and its final
  close report becomes the requesting executor task result.
- Each complex task request creates its initial task segment, and continuation
  may create later ordered segments.
- Retry stays inside the same segment and does not produce executor close reports
  until the complex task request closes.
- Partial-plan continuation creates the next task segment with `goal` set from
  the prior segment's `continuation_goal`.
