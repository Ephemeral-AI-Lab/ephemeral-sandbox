# Phase 04 - Complex Task Spawning and Partial Continuation

## Goal

Implement complex-task request creation and vertical task-segment continuation
after the durable model, orchestrators, and tool-gate foundations are in place.

Vertical motion creates new `TaskSegment`s inside one `ComplexTaskRequest`.
Horizontal retry creates new `HarnessGraph`s inside one segment.

All structural creation in this phase goes through
`ComplexTaskOrchestrator`.

## Creation paths

```
executor task E
  |
  +-- request_complex_task_solution(goal)
        ComplexTaskOrchestrator creates ComplexTaskRequest C
        C.requested_by_task_id = E
        ComplexTaskOrchestrator creates TaskSegment S1
        ComplexTaskOrchestrator creates HarnessGraph H1

TaskSegment S_n
  |
  +-- S_n closes with plan_shape = partial
        ComplexTaskOrchestrator creates TaskSegment S_n+1
        S_n+1.previous_segment_id = S_n
        ComplexTaskOrchestrator creates HarnessGraph H1 for S_n+1
```

`request_complex_task_solution` starts a new complex-task request. Partial-plan
continuation extends that same request.

## Field mapping

| Creation path | Entity created | Parent / lineage |
| ------------- | -------------- | ---------------- |
| `request_complex_task_solution` | `ComplexTaskRequest` | `requested_by_task_id` is the executor that called the tool |
| initial segment | `TaskSegment` | `complex_task_request_id = C`, `previous_segment_id = null`, `sequence_no = 1` |
| partial continuation | `TaskSegment` | `complex_task_request_id = C`, `previous_segment_id = S_n`, `sequence_no = n + 1` |
| initial graph | `HarnessGraph` | `task_segment_id = S`, `retry_no = 1` |
| retry graph | `HarnessGraph` | same `task_segment_id`, `retry_no = previous + 1` |

There is no `ROOT` spawn reason. Retry is not vertical motion.

## `request_complex_task_solution` workflow

```
Executor task E is running inside some harness graph

E calls request_complex_task_solution(goal)
    |
    v
ComplexTaskOrchestrator creates ComplexTaskRequest C
  requested_by_task_id = E
  goal                 = goal
    |
    v
ComplexTaskOrchestrator creates TaskSegment S1
    |
    v
ComplexTaskOrchestrator creates HarnessGraph S1.H1
    |
    v
HarnessGraphOrchestrator runs S1.H1 to completion
    |
    v
ComplexTaskOrchestrator handles retry, continuation, or request close
    |
    v
ComplexTaskOrchestrator delivers complex_task_succeeded or
complex_task_failed report
back to executor E
    |
    v
executor E resumes and eventually submits execution success or failure
```

`request_complex_task_solution` may happen at any graph depth and during any
generator executor task. Gating predicates that inspect partial-continuation
history use the new complex task request's segment chain, so a new request
starts with no prior partial segment.

## Partial-plan continuation workflow

```
planner in S1.H_k submits submit_partial_plan(
    dag,
    details,
    instructions_on_what_to_do_after_completion_of_partial_plan
)
    |
    v
S1.H_k runs its partial DAG
    |
    v
evaluator submits success
    |
    v
HarnessGraphOrchestrator marks S1.H_k passed with plan_shape = partial
and reports the graph outcome
    |
    v
ComplexTaskOrchestrator closes TaskSegment S1 with plan_shape = partial
    |
    v
ComplexTaskOrchestrator creates TaskSegment S2 because S1 closed partial
  complex_task_request_id = C
  previous_segment_id     = S1
  sequence_no             = 2
  goal                    = continuation instructions
    |
    v
ComplexTaskOrchestrator creates HarnessGraph S2.H1
    |
    v
planner in S2.H1 sees previous segment already used partial
submit_partial_plan is gated; planner must submit_full_plan
```

The complex task request stays open while continuation segments run. The
request closes only after a full-plan segment succeeds or a segment exhausts
retry budget and fails.

## Close reports

A `HarnessGraph` closes exactly once. Its outcome feeds the owning segment.
A `TaskSegment` closes exactly once. Its outcome either creates a continuation
segment, closes the request successfully, or closes the request as failed.

The complex-task close report returned to `requested_by_task_id` has these
harness-owned fields:

| Field | Meaning |
| ----- | ------- |
| `complex_task_request_id` | request id |
| `requested_by_task_id` | executor task that requested the complex solution |
| `outcome` | `success` or `failed` |
| `final_segment_id` | segment that produced the final outcome |
| `final_harness_graph_id` | harness graph that produced the final outcome |
| `plan_shape` | `full` or `partial` for the final successful graph when available |

Detailed payload such as per-task summaries, planner scratchpads, and evidence
links belongs to the context engine.

## Close-report routing

| Event | Routing |
| ----- | ------- |
| `ComplexTaskRequest` closes | report returns to the executor task that called `request_complex_task_solution`; that executor resumes from its paused state |
| `TaskSegment` closes with partial success | `ComplexTaskOrchestrator` creates the next segment because the previous segment closed partial; no report is returned to the requesting executor yet |
| `TaskSegment` closes with full success or failure | `ComplexTaskOrchestrator` closes the complex task request and returns one final report |

Retry never returns a close report to the requesting executor. Retry is
internal motion inside one task segment.

## Implementation tasks

1. Implement `request_complex_task_solution` creation of `ComplexTaskRequest`
   through `ComplexTaskOrchestrator`.
2. Pause and resume the calling executor around the complex-task close report.
3. Create initial `TaskSegment` and initial `HarnessGraph` through
   `ComplexTaskOrchestrator`.
4. Implement partial-continuation `TaskSegment` creation through
   `ComplexTaskOrchestrator`.
5. Set `previous_segment_id` on continuation segments.
6. Keep the complex task request open while continuation segments run.
7. Route continuation by creating the next segment rather than returning to the
   requesting executor.
8. Route final complex-task close reports back to the requesting executor.
9. Add close-report persistence or delivery semantics robust enough for
   process restart if the surrounding runtime supports it.

## Phase exit criteria

- `request_complex_task_solution` creates a complex task request and resumes
  the calling executor after the request closes.
- Partial-plan success creates the next task segment in the same request.
- Recursive partial plans are gated across previous segments.
- Retry still stays inside the same segment and does not produce executor close
  reports until the complex task request closes.
