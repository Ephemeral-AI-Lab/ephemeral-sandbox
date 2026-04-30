# Phase 02 - Harness Graph Orchestrator Lifecycle

## Goal

Move single-harness-graph execution decisions into `HarnessGraphOrchestrator`.

The lifecycle split is:

- `ComplexTaskRequestHandler` owns the request boundary and the segment chain:
  `request_complex_task_solution`, request creation, initial-segment creation,
  continuation-segment creation, executor pause/resume, request close, and
  final close-report delivery to `requested_by_task_id`.
- `TaskSegmentManager` is per-`TaskSegment` and owns harness-graph transitions
  inside one segment: retry-budget decisions, next-harness-graph creation
  after failed graphs, and segment close. It emits a `SegmentCloseReport` to
  `ComplexTaskRequestHandler` when its segment closes.
- `HarnessGraphOrchestrator` owns one `HarnessGraph` execution:
  `planner -> generator tasks -> evaluator`.

`HarnessGraphOrchestrator` is in-process and ephemeral. Durable state lives on
`ComplexTaskRequest`, `TaskSegment`, `HarnessGraph`, tasks, and task outputs.

## Responsibility Boundary

`HarnessGraphOrchestrator` never creates `ComplexTaskRequest`, `TaskSegment`,
or sibling `HarnessGraph` rows. It receives a current `HarnessGraph`, runs it
to a passed or failed outcome, and reports that outcome to its owning
`TaskSegmentManager` (one manager per segment).

`TaskSegmentManager` then decides, inside its owned segment, whether to:

- create another `HarnessGraph` after a failed graph when retry budget remains,
- close the current segment and emit a `SegmentCloseReport`.

`TaskSegmentManager` never creates a continuation `TaskSegment`. When a
passing graph closes the segment with a non-null `continuation_goal`, the
manager emits `success_continue(goal)` and `ComplexTaskRequestHandler`
creates the next segment (and its new `TaskSegmentManager`).

`ComplexTaskRequestHandler` closes the `ComplexTaskRequest` and resumes the
requesting executor only when it receives a `SegmentCloseReport` with
`success_terminal` or `failed_exhausted`.

## Harness Graph Orchestrator Responsibilities

For one `HarnessGraph`, `HarnessGraphOrchestrator`:

1. Spawns the harness graph planner.
2. Instantiates the generator DAG after a valid plan submission by creating
   generator task records and dependency edges.
3. Spawns generator tasks.
4. Watches generator terminal transitions.
5. Spawns the evaluator only after all generators pass.
6. Marks the harness graph passed or failed.
7. Reports the graph outcome to `TaskSegmentManager`.

## Harness graph stages

| Stage | Running work | Exit condition |
| ----- | ------------ | -------------- |
| `planning` | planner task | planner submits valid plan, or planner run ends without valid submission |
| `generating` | executor and verifier generator tasks | all generators are terminal |
| `evaluating` | evaluator task | evaluator submits success or failure |
| `closed` | none | harness graph is passed or failed |

Leaving `generating` does not always create an evaluator. If every generator is
`DONE`, `HarnessGraphOrchestrator` creates the evaluator and moves to
`evaluating`. If any generator is `FAILED` or `BLOCKED`, the graph closes as
failed after generator quiescence.

`request_complex_task_solution` may pause one executor while its complex task
request runs. That pause is executor-local waiting inside the executor's
current `generating` stage; it is not a separate harness graph stage.

## Failure escape valves

```
Failure escape valves:
  - Tool-call-level error from any agent
      prehook or handler returns ToolResult(is_error=True)
      -> agent retries inside its own run
      -> no harness-graph-level escalation

  - Generator or verifier submit_*_failure
      -> wait for generator quiescence
      -> mark HarnessGraph failed with generator_failed

  - Evaluator submit_evaluation_failure
      -> mark HarnessGraph failed with evaluator_failed immediately

  - Planner agent ends without a successful submit_*_plan
      -> runtime marks HarnessGraph failed with planner_step_budget_exhausted
```

The planner has no failure terminal. Its only soft-fail channel is inline
tool-call rejection. Only a planner run ending without a valid plan submission
escalates to `HarnessGraphOrchestrator` as
`planner_step_budget_exhausted`.

## Harness Graph Failures

| Failure mode | Detected by | Wait point |
| ------------ | ----------- | ---------- |
| `planner_step_budget_exhausted` | runtime ends planner without valid plan submission | immediate |
| `generator_failed` | executor or verifier submitted failure | wait until every generator is `DONE`, `FAILED`, or `BLOCKED` |
| `evaluator_failed` | evaluator submitted `submit_evaluation_failure` | immediate |

### Generator-failure quiescence

- When a generator fails, its dependents transition to `BLOCKED`.
- Independent sibling generators keep running.
- `HarnessGraphOrchestrator` does not retry mid-flight.
- After all generators are in `DONE`, `FAILED`, or `BLOCKED`,
  `HarnessGraphOrchestrator` makes one harness-graph-level outcome decision.
- If `TaskSegmentManager` spends retry budget, it creates the next harness
  graph; that graph's planner receives the whole failure landscape through the
  context engine.

### Evaluator failure

The evaluator is spawned only after every generator is `DONE`, so quiescence is
already satisfied. Evaluator failure triggers harness graph failure
immediately.

## Harness Graph Outcome

```
close_harness_graph(H, outcome):
    H.status            = passed | failed
    H.stage             = closed
    H.continuation_goal = null
                        | string (set from submit_partial_plan)
    H.fail_reason       = null
                        | planner_step_budget_exhausted
                        | generator_failed
                        | evaluator_failed

    TaskSegmentManager.handle_harness_graph_closed(H)
```

`H.continuation_goal` is set when the planner submits its plan, not at close.
`submit_full_plan` leaves it null; `submit_partial_plan(continuation_goal)`
sets it to the supplied goal. On failure paths it remains null. Each harness
graph's `continuation_goal` belongs to that graph alone — later graphs in the
same segment do not inherit it from prior graphs.

`HarnessGraphOrchestrator` does not inspect retry budget and does not create
the next graph. Retry is a segment-level decision owned by
`TaskSegmentManager`.

## Segment Reaction

`TaskSegmentManager` (per-segment) reacts to a closed harness graph. A passed
graph always closes its segment; a failed graph either retries within the
segment or closes the segment failed. The manager's only output is a
`SegmentCloseReport`:

```
H.status:
  passed
    segment.continuation_goal = H.continuation_goal
    close current segment.

    if segment.continuation_goal is None:
      emit SegmentCloseReport { outcome = success_terminal }
    else:
      emit SegmentCloseReport { outcome = success_continue(segment.continuation_goal) }

  failed
    if current segment has retry budget remaining:
      create HarnessGraph sequence N+1 in the same segment.
    else:
      close current segment failed.
      emit SegmentCloseReport { outcome = failed_exhausted(reason) }
```

A `TaskSegmentManager` retry creates a new `HarnessGraph` in the same
`TaskSegment`. The manager never creates a continuation segment, a new complex
task request, or another manager instance.

There is no policy hook for "spend retry on a passed graph": once a graph
passes, it closes the segment. Plan quality is enforced by the evaluator's
pass/fail decision, not by the segment manager.

`ComplexTaskRequestHandler` reacts to the `SegmentCloseReport`:

- `success_terminal` or `failed_exhausted` → close the complex task request and
  return the close report to `requested_by_task_id`.
- `success_continue(goal)` → create the next `TaskSegment` (with
  `previous_segment_id` and `goal` set), spawn a fresh `TaskSegmentManager`
  for it, and let that manager create the next segment's initial harness
  graph.

## Closure decision tree

```
HarnessGraphOrchestrator observes a terminal transition in HarnessGraph H
        |
        v
   H.stage:
        |
   +----+------------+----------------+
   v                 v                v
planning          generating       evaluating
   |                 |                |
   v                 v                v
planner ended      generators       evaluator submitted
without valid      quiescent?       success?
plan?              |                |
   |           +----+----+       +---+---+
   v           v         v       v       v
H failed     no        yes    H passed H failed
(planner_    |          |             (evaluator_failed)
 step_...)   v          v
          keep       any FAILED
          running    or BLOCKED?
                     |
                +----+----+
                v         v
             H failed  spawn evaluator
             (generator_failed)
```

## Implementation tasks

1. Add `HarnessGraphOrchestrator` lookup by `HarnessGraph.id`.
2. Route planner, generator, verifier, and evaluator terminal handlers through
   the current graph's `HarnessGraphOrchestrator`.
3. Implement planner success path: valid plan submission creates generator
   tasks and task dependencies for the current `HarnessGraph`.
4. Implement planner exhaustion path.
5. Implement generator failure quiescence and dependent blocking.
6. Implement evaluator spawn after generator success.
7. Implement evaluator success and failure handling.
8. Implement graph close reporting from `HarnessGraphOrchestrator` to
   `TaskSegmentManager`.
9. Keep continuation segment creation stubbed or feature-gated until Phase 04.

## Phase exit criteria

- A harness graph can complete a full-plan execution successfully.
- Generator failure waits for quiescence before graph failure is reported.
- Evaluator failure closes the harness graph immediately.
- Planner exhaustion closes the harness graph with
  `planner_step_budget_exhausted`.
- No retry path is implemented inside `HarnessGraphOrchestrator`; retry is
  delegated to `TaskSegmentManager`.
