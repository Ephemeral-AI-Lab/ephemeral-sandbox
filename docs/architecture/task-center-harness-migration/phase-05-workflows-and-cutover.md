# Phase 05 - End-to-End Workflows and Cutover

## Goal

Validate the full migration, remove obsolete graph-as-attempt behavior, and cut
callers over to the `ComplexTaskRequest` plus `TaskSegment` plus `HarnessGraph`
model.

Partial-plan continuation is removed. A complex task request has one segment,
and retry is represented by multiple harness graphs inside that segment.

## Happy path

```text
requesting executor starts
    |
    v
requesting executor decides task is non-atomic
    |
    v
requesting executor calls request_complex_task_solution(goal)
    |
    v
ComplexTaskRequestHandler creates ComplexTaskRequest C1
  requested_by_task_id = requesting executor
    |
    v
ComplexTaskRequestHandler creates TaskSegment S1
  and spawns TaskSegmentManager(S1)
    |
    v
TaskSegmentManager(S1) creates HarnessGraph S1.H1
    |
    v
HarnessGraphOrchestrator(S1.H1) spawns planner
    |
    v
planner submits submit_full_plan with task_specification,
evaluation_criteria, tasks, and task_specs
    |
    v
HarnessGraphOrchestrator(S1.H1) instantiates generator DAG and spawns generators
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
HarnessGraphOrchestrator(S1.H1) marks graph passed
    |
    v
TaskSegmentManager(S1) closes S1
TaskSegmentManager(S1) emits TaskSegmentClosureReport { outcome = terminal_success }
    |
    v
ComplexTaskRequestHandler closes C1 success
    |
    v
runtime delivers complex task success report to requested_by_task_id
```

## Segment-manager retry then pass path

```text
planner in S1.H1 submits a full plan; generators run; evaluator fails
(or planner exhausts, or generator fails)
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed
    |
    v
TaskSegmentManager decides retry budget remains
TaskSegmentManager creates next HarnessGraph S1.H2
    |
    v
planner in S1.H2 submits submit_full_plan
    |
    v
S1.H2 runs to pass
    |
    v
TaskSegmentManager closes S1 successfully
ComplexTaskRequestHandler closes C1 successfully
```

## Resolver loop validation

The resolver loop remains inside one `HarnessGraph`:

```text
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

```text
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
and reports failure to TaskSegmentManager
    |
    +-- TaskSegmentManager: retry budget remains -> create next graph S1.H2
    |
    +-- TaskSegmentManager: retry exhausted      -> emit attempt_plan_failed(attempted_plan_history)
                                                    ComplexTaskRequestHandler closes C1 failed
```

### Evaluator failure

```text
evaluator in S1.H1 submits failure
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed with evaluator_failed
and reports failure to TaskSegmentManager
    |
    +-- TaskSegmentManager: retry budget remains -> create next graph S1.H2
    |
    +-- TaskSegmentManager: retry exhausted      -> emit attempt_plan_failed(attempted_plan_history)
                                                    ComplexTaskRequestHandler closes C1 failed
```

### Planner exhaustion

```text
planner in S1.H1 ends without valid full-plan submission
    |
    v
runtime reports planner_step_budget_exhausted
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed
and reports failure to TaskSegmentManager
    |
    +-- TaskSegmentManager: retry budget remains -> create next graph S1.H2
    |
    +-- TaskSegmentManager: retry exhausted      -> emit attempt_plan_failed(attempted_plan_history)
                                                    ComplexTaskRequestHandler closes C1 failed
```

## Cutover sequence

1. Add feature flags or compatibility adapters if needed so old tests can run
   while the new model lands.
2. Add `ComplexTaskRequestHandler` for request creation and close-report
   delivery.
3. Migrate persistence and stores from graph-as-attempt to
   `ComplexTaskRequest` / `TaskSegment` / `HarnessGraph`.
4. Migrate graph terminal handlers to `HarnessGraphOrchestrator` routing and
   request decisions to `ComplexTaskRequestHandler` plus segment decisions to
   `TaskSegmentManager`.
5. Migrate retry from attempt rows or child graph spawn to
   `TaskSegmentManager` creation of the next `HarnessGraph` inside the same
   segment after a failed graph.
6. Migrate `submit_request_plan` to `request_complex_task_solution`.
7. Remove `submit_partial_plan` and all partial-plan compatibility paths.
8. Migrate tool gates to read request, segment, and harness graph state.
9. Update prompts and docs that mention retry as a child graph or
   `RETRY_ON_FAILURE`.
10. Remove obsolete attempt rows, old graph attempt state, old spawn reasons, the
    obsolete persisted `plan_shape` field, old persisted
    `final_harness_graph_id` fields, `retry_after_partial`, and compatibility
    code.
11. Run targeted TaskCenter runtime tests, then broader backend checks.

The `final_harness_graph_id` in `TaskSegmentClosureReport` remains valid as an
event payload. The removal item above refers only to obsolete persisted fields
from the old graph-as-attempt model.

## Test plan

Prioritize focused tests near the runtime modules touched by the migration.

Minimum coverage:

- `request_complex_task_solution` creates `ComplexTaskRequest`.
- The complex-task close report becomes the requesting executor task result.
- `ComplexTaskRequestHandler` is the only creator and closer for requests, and
  the only creator of the single `TaskSegment` record.
- `TaskSegmentManager` is per-segment, the only creator of `HarnessGraph`
  records inside its owned segment, and the only emitter of `TaskSegmentClosureReport`.
- Request links to `requested_by_task_id`.
- Initial `TaskSegment` creation.
- Initial `HarnessGraph` creation.
- Full-plan happy path.
- Generator failure quiescence.
- Evaluator failure triggers a `TaskSegmentManager` retry decision.
- Planner exhaustion triggers a `TaskSegmentManager` retry decision.
- Retry budget exhaustion.
- `TaskSegmentManager` retry creates `HarnessGraph` N+1 inside the same segment.
- A passing harness graph always closes its segment; failed graphs return to
  `TaskSegmentManager` for a retry decision subject to budget.
- `request_complex_task_solution` can create a nested `ComplexTaskRequest` from
  a generator executor inside an existing harness graph.
- No `submit_partial_plan` tool or partial-plan request path remains.
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

1. Retry-budget defaults for task segments: fixed runtime defaults or
   request-level configuration?
2. Planner step-budget detection: confirm the exact runtime signal for
   `planner_step_budget_exhausted`.

## Phase exit criteria

- All phase tests pass.
- Public executor contract exposes `request_complex_task_solution`,
  `submit_execution_success`, and `submit_execution_failure`.
- Public planner contract exposes `submit_full_plan` only.
- Docs no longer describe retry as `RETRY_ON_FAILURE` child graph creation.
- Segment state reflects one request-local retry scope.
- Retry history is derived from ordered harness graphs inside one segment.
