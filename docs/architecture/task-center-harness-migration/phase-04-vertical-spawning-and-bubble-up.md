# Phase 04 - Complex Task Spawning and Continuation

## Goal

Implement complex-task request creation and vertical task-segment continuation
after the durable model, orchestrators, and tool-gate foundations are in place.

Vertical motion creates new `TaskSegment`s inside one `ComplexTaskRequest`.
Segment-manager retry creates new `HarnessGraph`s inside one segment.

`ComplexTaskRequestHandler` is the only creator of `ComplexTaskRequest` and
`TaskSegment` records, and the only spawner of `TaskSegmentManager` instances.
Each per-segment `TaskSegmentManager` is the only creator of `HarnessGraph`
records inside its owned segment.

## Creation paths

```
executor task E
  |
  +-- request_complex_task_solution(goal)
        ComplexTaskRequestHandler creates ComplexTaskRequest C
          C.requested_by_task_id = E
        ComplexTaskRequestHandler creates TaskSegment S1
          S1.goal = C.goal
        ComplexTaskRequestHandler spawns TaskSegmentManager(S1)
        TaskSegmentManager(S1) creates HarnessGraph H1

TaskSegment S_n closes with continuation_goal != null
  |
  +-- TaskSegmentManager(S_n) emits SegmentCloseReport { success_continue(g) }
        ComplexTaskRequestHandler creates TaskSegment S_n+1
          S_n+1.previous_segment_id = S_n
          S_n+1.goal = g
        ComplexTaskRequestHandler spawns TaskSegmentManager(S_n+1)
        TaskSegmentManager(S_n+1) creates HarnessGraph H1
```

`request_complex_task_solution` starts a new complex-task request. Continuation
extends that same request.

Continuation is based on the segment's `continuation_goal`, which is set only
from the passing harness graph that closes the segment.

## Field mapping

| Creation path | Entity created | Created by | Parent / lineage |
| ------------- | -------------- | ---------- | ---------------- |
| `request_complex_task_solution` | `ComplexTaskRequest` | `ComplexTaskRequestHandler` | `requested_by_task_id` is the executor that called the tool |
| initial segment | `TaskSegment` | `ComplexTaskRequestHandler` | `complex_task_request_id = C`, `previous_segment_id = null`, `sequence_no = 1`, `goal = C.goal` |
| continuation | `TaskSegment` | `ComplexTaskRequestHandler` (on `success_continue` from `TaskSegmentManager(S_n)`) | `complex_task_request_id = C`, `previous_segment_id = S_n`, `sequence_no = n + 1`, `goal = S_n.continuation_goal` |
| initial graph | `HarnessGraph` | `TaskSegmentManager(S)` | `task_segment_id = S`, `graph_sequence_no = 1` |
| subsequent graph after failed graph | `HarnessGraph` | `TaskSegmentManager(S)` | same `task_segment_id`, `graph_sequence_no = previous + 1`; created only after the manager decides to spend retry budget |

There is no `ROOT` spawn reason. Retry is not vertical motion and is not a
`HarnessGraph` creation reason. A later graph's `continuation_goal` is decided
independently by its own planner; it is not inherited from the prior failed
graph.

## `request_complex_task_solution` workflow

```
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
TaskSegmentManager(S1) retries inside S1, or closes S1 and emits SegmentCloseReport
    |
    v
ComplexTaskRequestHandler routes the report:
  success_terminal | failed_exhausted -> close request
  success_continue(goal)              -> create S2 and spawn TaskSegmentManager(S2)
    |
    v
ComplexTaskRequestHandler delivers complex_task_succeeded or
complex_task_failed report
back to executor E
    |
    v
executor E resumes and eventually submits execution success or failure
```

`request_complex_task_solution` may happen at any graph depth and during any
generator executor task. Gating predicates that inspect continuation history
use the new complex task request's segment chain, so a new request starts with
no prior continuation history.

## Recursive complex-task requests

Complex-task requests are recursive. Any generator executor running inside a
`HarnessGraph` can call `request_complex_task_solution(goal)` before it edits.
That call creates a new `ComplexTaskRequest` whose `requested_by_task_id` is
the executor task that called the tool.

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
ComplexTaskRequestHandler returns C2 close report to E7
  |
  v
E7 resumes inside C1.S1.H1
```

Only the requesting executor is paused. The nested request does not become a
child `TaskSegment` of the outer request, and it does not use the outer
request's continuation history.

## Partial-plan continuation workflow

```
planner in S1.H_k submits submit_partial_plan(
    task_specification,
    evaluation_criteria,
    tasks,
    task_specs,
    continuation_goal = G
)
    |
    v
S1.H_k.continuation_goal = G          (set on this graph only)
    |
    v
S1.H_k runs its partial DAG
    |
    v
evaluator submits success
    |
    v
HarnessGraphOrchestrator marks S1.H_k passed
and reports the graph outcome to TaskSegmentManager(S1)
    |
    v
TaskSegmentManager(S1) closes TaskSegment S1
S1.continuation_goal = S1.H_k.continuation_goal = G
emits SegmentCloseReport { outcome = success_continue(G) }
    |
    v
ComplexTaskRequestHandler creates TaskSegment S2 because outcome is success_continue
  complex_task_request_id = C
  previous_segment_id     = S1
  sequence_no             = 2
  goal                    = G
spawns TaskSegmentManager(S2)
    |
    v
TaskSegmentManager(S2) creates HarnessGraph S2.H1
    |
    v
planner in S2.H1 sees previous segment already used a partial plan
submit_partial_plan is gated; planner must submit_full_plan
```

The complex task request stays open while continuation segments run. The
request closes only after a terminal segment succeeds (passing graph with
`continuation_goal = null`) or a segment exhausts retry budget and fails.

## Segment continuation source of truth

`ComplexTaskRequestHandler` creates `TaskSegment N+1` only when
`TaskSegmentManager(S_n)` emits `SegmentCloseReport` with outcome
`success_continue(goal)`. The segment's `continuation_goal` is inherited from
the harness graph that closed the segment — which is the passing (last
successful) harness graph in that segment, since failed graphs return to
`TaskSegmentManager` for a retry decision rather than closing.
`TaskSegmentManager` itself never creates the next segment.

Each harness graph's `continuation_goal` is set independently by its own
planner submission. A later graph in the same segment does not inherit
`continuation_goal` from prior failed graphs. The segment learns its
`continuation_goal` only when one of its harness graphs passes.

## Close reports

A `HarnessGraph` closes exactly once. Its outcome feeds the owning segment.
A `TaskSegment` closes exactly once. Its close report either causes
`ComplexTaskRequestHandler` to create a continuation segment, close the
request successfully, or close the request as failed.

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
| `ComplexTaskRequest` closes | report returns to the executor task that called `request_complex_task_solution`; that executor resumes from its paused state |
| `TaskSegment` closes with non-null `continuation_goal` | `TaskSegmentManager` emits `success_continue(goal)`; `ComplexTaskRequestHandler` creates the next segment and spawns its `TaskSegmentManager`; no report is returned to the requesting executor yet |
| `TaskSegment` closes with null `continuation_goal` (terminal) or as failed | `TaskSegmentManager` emits `success_terminal` or `failed_exhausted`; `ComplexTaskRequestHandler` closes the complex task request and returns one final report |

Retry never returns a close report to the requesting executor. Retry is
internal motion inside one task segment.

## Implementation tasks

1. Implement `request_complex_task_solution` creation of `ComplexTaskRequest`
   through `ComplexTaskRequestHandler`.
2. Pause and resume the calling executor around the complex-task close report.
3. Create initial `TaskSegment` through `ComplexTaskRequestHandler`, spawn
   `TaskSegmentManager(S1)`, then have the manager create the initial
   `HarnessGraph`.
4. Implement continuation `TaskSegment` creation through
   `ComplexTaskRequestHandler` when it receives `success_continue(goal)` from
   `TaskSegmentManager(S_n)`; spawn a fresh `TaskSegmentManager` for the new
   segment.
5. Set `previous_segment_id` and `goal` on continuation segments.
6. Keep the complex task request open while continuation segments run.
7. Route continuation by creating the next segment rather than returning to the
   requesting executor.
8. Route final complex-task close reports back to the requesting executor.
9. Add close-report persistence or delivery semantics robust enough for
   process restart if the surrounding runtime supports it.

## Phase exit criteria

- `request_complex_task_solution` creates a complex task request and resumes
  the calling executor after the request closes.
- A passing harness graph with non-null `continuation_goal` closes its segment
  and creates the next task segment in the same request.
- A later harness graph's `continuation_goal` is set only by its own planner
  and not inherited from prior failed graphs.
- Recursive partial plans are gated across previous segments.
- Retry stays inside the same segment and does not produce executor close
  reports until the complex task request closes.
