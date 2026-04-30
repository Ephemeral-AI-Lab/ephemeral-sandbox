# Phase 05 - End-to-End Workflows and Cutover

## Goal

Validate the full migration, remove obsolete graph-as-attempt behavior, and cut
callers over to the `ComplexTaskRequest` plus `TaskSegment` plus
`HarnessGraph` model.

## Happy path

```
root executor starts
    |
    v
root executor decides task is non-atomic
    |
    v
root executor calls request_complex_task_solution(goal)
    |
    v
ComplexTaskOrchestrator creates ComplexTaskRequest C1
  requested_by_task_id = root executor
    |
    v
ComplexTaskOrchestrator creates TaskSegment S1
    |
    v
ComplexTaskOrchestrator creates HarnessGraph S1.H1
    |
    v
HarnessGraphOrchestrator(S1.H1) spawns planner
    |
    v
planner submits submit_full_plan
    |
    v
HarnessGraphOrchestrator(S1.H1) materializes DAG and spawns generators
    |
    v
executors and verifiers submit success
    |
    v
HarnessGraphOrchestrator(S1.H1) spawns evaluator
    |
    v
evaluator submits success
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph passed with plan_shape = full
    |
    v
ComplexTaskOrchestrator closes S1 success and C1 success
    |
    v
runtime delivers complex task success report to root executor
    |
    v
root executor continues or submits final execution terminal
    |
    v
root executor closes; session ends
```

## Partial continuation path

```
planner in S1.H1 submits submit_partial_plan
    |
    v
generators complete partial DAG
    |
    v
evaluator submits success
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph passed with plan_shape = partial
    |
    v
ComplexTaskOrchestrator closes S1 with plan_shape = partial
    |
    v
ComplexTaskOrchestrator creates TaskSegment S2 because S1 closed partial
  previous_segment_id = S1
    |
    v
ComplexTaskOrchestrator creates HarnessGraph S2.H1
    |
    v
planner in S2.H1 must submit_full_plan
    |
    v
HarnessGraphOrchestrator(S2.H1) runs graph to full-plan pass
    |
    v
ComplexTaskOrchestrator closes S2 and C1, then returns one final result to
requested_by_task_id
```

## Resolver loop validation

The resolver loop remains inside one `HarnessGraph`:

```
verifier or evaluator calls ask_resolver(issues)
    |
    v
resolver runs and may edit
    |
    v
resolver returns resolved plus summaries
    |
    v
caller re-checks
    |
    +-- resolved true  -> may submit success
    |
    +-- resolved false -> unresolved count increments
                         at five unresolved calls, success terminal is blocked
                         caller must submit failure
```

## Failure workflow validation

### Generator failure

```
generator in S1.H1 submits failure
    |
    v
dependent generators become BLOCKED
    |
    v
independent generators keep running
    |
    v
generators become quiescent
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed with generator_failed
and reports failure to ComplexTaskOrchestrator
    |
    +-- ComplexTaskOrchestrator: retry budget remains -> create S1.H2
    |
    +-- ComplexTaskOrchestrator: retry exhausted      -> close S1 failed, close C1 failed, return report
```

### Evaluator failure

```
evaluator in S1.H1 submits failure
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed with evaluator_failed
and reports failure to ComplexTaskOrchestrator
    |
    +-- ComplexTaskOrchestrator: retry budget remains -> create S1.H2
    |
    +-- ComplexTaskOrchestrator: retry exhausted      -> close S1 failed, close C1 failed, return report
```

### Planner exhaustion

```
planner in S1.H1 ends without valid plan submission
    |
    v
runtime reports planner_step_budget_exhausted
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed
and reports failure to ComplexTaskOrchestrator
    |
    +-- ComplexTaskOrchestrator: retry budget remains -> create S1.H2
    |
    +-- ComplexTaskOrchestrator: retry exhausted      -> close S1 failed, close C1 failed, return report
```

## Cutover sequence

1. Add feature flags or compatibility adapters if needed so old tests can run
   while the new model lands.
2. Add `ComplexTaskOrchestrator` as the single structural creation path.
3. Migrate persistence and stores from graph-as-attempt to
   `ComplexTaskRequest` / `TaskSegment` / `HarnessGraph`.
4. Migrate graph terminal handlers to `HarnessGraphOrchestrator` routing and
   request/segment decisions to `ComplexTaskOrchestrator`.
5. Migrate retry from attempt rows or child graph spawn to next
   `HarnessGraph` spawn inside the same segment.
6. Migrate `submit_request_plan` to `request_complex_task_solution`.
7. Migrate partial-plan continuation to `TaskSegment` creation with
   `previous_segment_id` lineage.
8. Migrate tool gates to read request, segment, and harness graph state.
9. Update prompts and docs that mention retry as a child graph or
   `RETRY_ON_FAILURE`.
10. Remove obsolete attempt rows, retry graph states, old spawn reasons, and
   compatibility code.
11. Run targeted TaskCenter runtime tests, then broader backend checks.

## Test plan

Prioritize focused tests near the runtime modules touched by the migration.

Minimum coverage:

- Root executor creation and closure.
- `request_complex_task_solution` creates `ComplexTaskRequest`.
- `ComplexTaskOrchestrator` is the only creator for requests, segments, and
  harness graphs.
- Request links to `requested_by_task_id`.
- Initial `TaskSegment` creation.
- Initial `HarnessGraph` creation.
- Full-plan happy path.
- Generator failure quiescence.
- Evaluator failure retry.
- Planner exhaustion retry.
- Retry budget exhaustion.
- Retry creates `HarnessGraph` N+1 inside the same segment.
- Partial-plan continuation creates `TaskSegment` N+1.
- Recursive partial-plan gate blocks continuation planners.
- Complex-task close report resumes the requesting executor.
- No `RETRY_ON_FAILURE` graph spawn remains.
- No `ROOT` spawn or creation reason remains.

Suggested commands:

```bash
uv run pytest backend/tests/test_task_center -q
uv run pytest backend/tests/test_engine -q
uv run ruff check backend/src backend/tests
uv run mypy --config-file backend/mypy.ini backend/src/task_center backend/src/agents
```

## Open questions before final cutover

1. Retry-budget defaults for task segments: fixed runtime defaults, request
   configuration, or continuation override?
2. Parent-while-request-runs state: confirm that a paused executor waiting for
   a complex-task result does not require a separate harness graph stage.
3. Planner step-budget detection: confirm the exact runtime signal for
   `planner_step_budget_exhausted`.
4. Context-engine boundary: planner launch context, per-harness-graph evidence,
   detailed close-report payloads, and prior-segment visibility need their own
   spec.

## Phase exit criteria

- All phase tests pass.
- Public executor contract exposes `request_complex_task_solution`,
  `submit_execution_success`, and `submit_execution_failure`.
- Docs no longer describe retry as `RETRY_ON_FAILURE` child graph creation.
- Segment progression reflects only partial-plan continuation.
- Retry history is stored only as harness graphs inside one segment.
