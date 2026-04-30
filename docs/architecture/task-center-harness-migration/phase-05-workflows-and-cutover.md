# Phase 05 - End-to-End Workflows and Cutover

## Goal

Validate the full migration, remove obsolete graph-as-attempt behavior, and cut
callers over to the `ComplexTaskRequest` plus `TaskSegment` plus
`HarnessGraph` model.

## Happy path

```
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
S1.H1.continuation_goal = null
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
S1.continuation_goal = S1.H1.continuation_goal = null
TaskSegmentManager(S1) emits SegmentCloseReport { outcome = success_terminal }
    |
    v
ComplexTaskRequestHandler closes C1 success
    |
    v
runtime delivers complex task success report to requesting executor
    |
    v
requesting executor continues or submits final execution terminal
    |
    v
requesting executor closes its task
```

## Partial continuation path

```
planner in S1.H1 submits submit_partial_plan with task_specification,
evaluation_criteria, tasks, task_specs, and continuation_goal = G
    |
    v
S1.H1.continuation_goal = G          (per-graph; not shared with later graphs)
    |
    v
generators complete partial DAG
    |
    v
evaluator submits success
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph passed
    |
    v
TaskSegmentManager(S1) closes S1
S1.continuation_goal = S1.H1.continuation_goal = G
    (segment inherits from the passing harness graph)
TaskSegmentManager(S1) emits SegmentCloseReport { outcome = success_continue(G) }
    |
    v
ComplexTaskRequestHandler creates TaskSegment S2 because outcome is success_continue
  previous_segment_id = S1
  goal                = G
ComplexTaskRequestHandler spawns TaskSegmentManager(S2)
    |
    v
TaskSegmentManager(S2) creates HarnessGraph S2.H1
    |
    v
planner in S2.H1 must submit_full_plan (recursive partial gate)
    |
    v
HarnessGraphOrchestrator(S2.H1) runs graph to full-plan pass
S2.H1.continuation_goal = null
    |
    v
TaskSegmentManager(S2) closes S2 (S2.continuation_goal = null)
emits SegmentCloseReport { outcome = success_terminal }
    |
    v
ComplexTaskRequestHandler closes C1 and returns one final result to
requested_by_task_id
```

## Segment-manager retry then pass path

```
planner in S1.H1 submits a plan; generators run; evaluator fails (or planner
exhausts, or generator fails)
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed
    |
    v
TaskSegmentManager decides retry budget remains
TaskSegmentManager creates next HarnessGraph S1.H2
    (S1.H2.continuation_goal starts unset; its own planner will decide)
    |
    v
planner in S1.H2 submits submit_full_plan or submit_partial_plan
    (independent decision; S1.H1's continuation_goal is not inherited)
    |
    v
S1.H2 runs to pass
    |
    v
TaskSegmentManager closes S1
S1.continuation_goal = S1.H2.continuation_goal
    (only the passing graph contributes)
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
and reports failure to TaskSegmentManager
    |
    +-- TaskSegmentManager: retry budget remains -> create next graph S1.H2
    |
    +-- TaskSegmentManager: retry exhausted      -> emit failed_exhausted
                                                    ComplexTaskRequestHandler closes C1 failed
```

### Evaluator failure

```
evaluator in S1.H1 submits failure
    |
    v
HarnessGraphOrchestrator(S1.H1) marks graph failed with evaluator_failed
and reports failure to TaskSegmentManager
    |
    +-- TaskSegmentManager: retry budget remains -> create next graph S1.H2
    |
    +-- TaskSegmentManager: retry exhausted      -> emit failed_exhausted
                                                    ComplexTaskRequestHandler closes C1 failed
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
and reports failure to TaskSegmentManager
    |
    +-- TaskSegmentManager: retry budget remains -> create next graph S1.H2
    |
    +-- TaskSegmentManager: retry exhausted      -> emit failed_exhausted
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
7. Migrate partial-plan continuation to `TaskSegment` creation with
   `previous_segment_id` lineage and `continuation_goal` inherited from the
   passing harness graph.
8. Migrate tool gates to read request, segment, and harness graph state.
9. Update prompts and docs that mention retry as a child graph or
   `RETRY_ON_FAILURE`.
10. Remove obsolete attempt rows, retry graph states, old spawn reasons, the
    `plan_shape` field, the `closing_harness_graph_id` field,
    `retry_after_partial`, and compatibility code.
11. Run targeted TaskCenter runtime tests, then broader backend checks.

## Test plan

Prioritize focused tests near the runtime modules touched by the migration.

Minimum coverage:

- Requesting executor pause, resume, and terminal closure.
- `request_complex_task_solution` creates `ComplexTaskRequest`.
- `ComplexTaskRequestHandler` is the only creator and closer for requests, and
  the only creator of `TaskSegment` records (initial and continuation).
- `TaskSegmentManager` is per-segment, the only creator of `HarnessGraph`
  records inside its owned segment, and the only emitter of
  `SegmentCloseReport`.
- A passing graph with `continuation_goal != null` produces a
  `success_continue` report from the manager and `ComplexTaskRequestHandler`
  creates the next segment plus a fresh manager.
- Request links to `requested_by_task_id`.
- Initial `TaskSegment` creation.
- Initial `HarnessGraph` creation.
- Full-plan happy path.
- Generator failure quiescence.
- Evaluator failure triggers a `TaskSegmentManager` retry decision.
- Planner exhaustion triggers a `TaskSegmentManager` retry decision.
- Retry budget exhaustion.
- `TaskSegmentManager` retry creates `HarnessGraph` N+1 inside the same
  segment.
- A later harness graph's `continuation_goal` is set independently by its own
  planner and is not inherited from prior failed graphs.
- Continuation creates `TaskSegment` N+1 with `goal` inherited from the
  previous segment's `continuation_goal`, which itself was inherited from the
  passing harness graph that closed the previous segment.
- A passing harness graph always closes its segment; failed graphs return to
  `TaskSegmentManager` for a retry decision subject to budget.
- `request_complex_task_solution` can create a nested `ComplexTaskRequest`
  from a generator executor inside an existing harness graph.
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

## Phase exit criteria

- All phase tests pass.
- Public executor contract exposes `request_complex_task_solution`,
  `submit_execution_success`, and `submit_execution_failure`.
- Docs no longer describe retry as `RETRY_ON_FAILURE` child graph creation.
- Segment progression reflects only continuation through `continuation_goal`
  inherited from the passing harness graph.
- Retry history is derived from ordered harness graphs inside one segment,
  with per-graph `continuation_goal` independence.
