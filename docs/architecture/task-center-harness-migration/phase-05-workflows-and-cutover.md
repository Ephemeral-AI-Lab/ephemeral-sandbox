# Phase 05 - End-to-End Workflows and Cutover

## Goal

Validate the full migration, remove obsolete graph-spawn retry behavior, and
cut callers over to the recursive graph plus graph-local Attempt model.

## Happy path

```
G_root.Orch initializes root executor
    |
    v
root executor calls submit_request_plan(note)
    |
    v
G_root.Orch spawns child G1 with spawn_reason = REQUEST_PLAN
    |
    v
G1.Orch creates Attempt 1 and spawns planner
    |
    v
planner submits submit_full_plan
    |
    v
G1.Orch materializes DAG and spawns generators
    |
    v
executors and verifiers submit success
    |
    v
G1.Orch spawns evaluator
    |
    v
evaluator submits success
    |
    v
G1.Orch marks Attempt 1 passed and closes G1 success
    |
    v
G_root.Orch delivers child success to root executor
    |
    v
root executor continues or submits final execution terminal
    |
    v
G_root closes; session ends
```

## Resolver loop validation

The resolver loop remains inside one Attempt:

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
generator in G1.A1 submits failure
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
G1.Orch marks A1 failed with generator_failed
    |
    +-- retry budget remains -> spawn Attempt 2 inside G1
    |
    +-- retry exhausted      -> close G1 failed and bubble up
```

### Evaluator failure

```
evaluator in G1.A1 submits failure
    |
    v
G1.Orch marks A1 failed with evaluator_failed
    |
    +-- retry budget remains -> spawn Attempt 2 inside G1
    |
    +-- retry exhausted      -> close G1 failed and bubble up
```

### Planner exhaustion

```
planner in G1.A1 ends without valid plan submission
    |
    v
runtime reports planner_step_budget_exhausted
    |
    v
G1.Orch marks A1 failed
    |
    +-- retry budget remains -> spawn Attempt 2 inside G1
    |
    +-- retry exhausted      -> close G1 failed and bubble up
```

## Cutover sequence

1. Add feature flags or compatibility adapters if needed so old tests can run
   while the new model lands.
2. Migrate persistence and stores from graph-as-attempt to graph-with-Attempts.
3. Migrate terminal handlers to local orchestrator routing.
4. Migrate retry from child graph spawn to next Attempt spawn.
5. Migrate `REQUEST_PLAN` to child graph creation with executor pause/resume.
6. Migrate partial-plan continuation to child graph creation with
   `prior_graph_id` lineage.
7. Migrate tool gates to read graph and Attempt state.
8. Update prompts and docs that mention retry as a child graph or
   `RETRY_ON_FAILURE`.
9. Remove obsolete retry graph states, fields, and compatibility code.
10. Run targeted team runtime tests, then broader backend checks.

## Test plan

Prioritize focused tests near the runtime modules touched by the migration.

Minimum coverage:

- Root graph creation and closure.
- Non-root graph Attempt creation.
- Full-plan happy path.
- Generator failure quiescence.
- Evaluator failure retry.
- Planner exhaustion retry.
- Retry budget exhaustion.
- `REQUEST_PLAN` child graph spawn and executor resume.
- `REQUEST_PLAN` resets `prior_graph_id`.
- Partial-plan continuation sets `prior_graph_id`.
- Recursive partial-plan gate blocks continuation planners.
- Close-report routing for request-plan and continuation children.
- No `RETRY_ON_FAILURE` graph spawn remains.

Suggested commands:

```bash
uv run pytest backend/tests/team -q
uv run pytest backend/tests/test_engine -q
uv run ruff check backend/src backend/tests
uv run mypy --config-file backend/mypy.ini backend/src/team backend/src/agents
```

## Open questions before final cutover

1. Retry-budget defaults for `REQUEST_PLAN` and
   `CONTINUE_AFTER_PARTIAL_PLAN` children: fixed runtime defaults or
   configurable per graph?
2. Parent-while-child-runs state: confirm that a paused executor waiting for a
   child graph does not require a separate Attempt stage.
3. Planner step-budget detection: confirm the exact runtime signal for
   `planner_step_budget_exhausted`.
4. Context-engine boundary: planner launch context, per-Attempt evidence,
   detailed close-report payloads, and prior-segment visibility need their
   own spec.

## Phase exit criteria

- All phase tests pass.
- Docs no longer describe retry as `RETRY_ON_FAILURE` child graph creation.
- Public agent contracts still expose the intended terminal tools.
- Graph depth reflects only delegation and partial-plan continuation.
- Retry history is stored only as Attempts inside one graph.
