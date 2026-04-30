# Phase 02 - Harness Graph Orchestrator Lifecycle

## Goal

Move one-harness-graph execution decisions into `HarnessGraphOrchestrator`.

The orchestration split is:

- `ComplexTaskOrchestrator` owns complex-task request lifecycle, task segment
  lifecycle, structural creation, retry decisions, partial continuation, and
  final close-report delivery to `requested_by_task_id`.
- `HarnessGraphOrchestrator` owns one `HarnessGraph` execution:
  `planner -> generator tasks -> evaluator`.

`HarnessGraphOrchestrator` is in-process and ephemeral. Durable state lives on
`ComplexTaskRequest`, `TaskSegment`, `HarnessGraph`, tasks, and task outputs.

## Responsibility Boundary

`HarnessGraphOrchestrator` never creates `ComplexTaskRequest`, `TaskSegment`,
or `HarnessGraph` rows. It receives a current `HarnessGraph`, runs it to a
passed or failed outcome, and reports that outcome to `ComplexTaskOrchestrator`.

`ComplexTaskOrchestrator` then decides whether to:

- create a retry `HarnessGraph`,
- create a partial-continuation `TaskSegment`,
- close the current segment,
- close the complex task request.

## Harness Graph Orchestrator Responsibilities

For one `HarnessGraph`, `HarnessGraphOrchestrator`:

1. Spawns the harness graph planner.
2. Materializes generator tasks and task dependencies after a valid plan
   submission.
3. Spawns generator tasks.
4. Watches generator terminal transitions.
5. Spawns the evaluator only after all generators pass.
6. Marks the harness graph passed or failed.
7. Reports the graph outcome to `ComplexTaskOrchestrator`.

## Harness graph stages

| Stage | Running work | Exit condition |
| ----- | ------------ | -------------- |
| `planning` | planner task | planner submits valid plan, or planner run ends without valid submission |
| `generating` | executor and verifier generator tasks | all generators are terminal |
| `evaluating` | evaluator task | evaluator submits success or failure |
| `closed` | none | harness graph is passed or failed |

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
- If retry remains, `ComplexTaskOrchestrator` creates the next harness graph;
  that graph's planner receives the whole failure landscape through the
  context engine.

### Evaluator failure

The evaluator is spawned only after every generator is `DONE`, so quiescence is
already satisfied. Evaluator failure triggers harness graph failure
immediately.

## Harness Graph Outcome

```
close_harness_graph(H, outcome):
    H.status      = passed | failed
    H.stage       = closed
    H.plan_shape  = full | partial | null
    H.fail_reason = null
                  | planner_step_budget_exhausted
                  | generator_failed
                  | evaluator_failed

    ComplexTaskOrchestrator.handle_harness_graph_closed(H)
```

`HarnessGraphOrchestrator` does not inspect retry budget and does not create
the next graph. Retry is a segment-level decision owned by
`ComplexTaskOrchestrator`.

## Complex Task Reaction

`ComplexTaskOrchestrator` reacts to a closed harness graph:

```
H.status:
  passed
    if H.plan_shape == partial:
      close current segment with plan_shape = partial.
      create TaskSegment N+1.
      create HarnessGraph retry 1 in the new segment.
    else:
      close current segment success.
      close ComplexTaskRequest success.
      return close report to requested_by_task_id.

  failed
    if current segment has retry budget remaining:
      create HarnessGraph retry N+1 in the same segment.
    else:
      close current segment failed.
      close ComplexTaskRequest failed.
      return close report to requested_by_task_id.
```

Retry creates a new `HarnessGraph` in the same `TaskSegment`. It never creates
a new segment or complex task request.

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
   `ComplexTaskOrchestrator`.
9. Keep partial-continuation segment creation stubbed or feature-gated until
   Phase 04.

## Phase exit criteria

- A harness graph can complete a full-plan execution successfully.
- Generator failure waits for quiescence before graph failure is reported.
- Evaluator failure closes the harness graph immediately.
- Planner exhaustion closes the harness graph with
  `planner_step_budget_exhausted`.
- No retry path is implemented inside `HarnessGraphOrchestrator`; retry is
  delegated to `ComplexTaskOrchestrator`.
