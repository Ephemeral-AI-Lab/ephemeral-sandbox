# Phase 02 - Local Orchestrator Lifecycle

## Goal

Move graph lifecycle decisions into one local orchestrator per
`HarnessGraph`. The orchestrator drives both axes:

- horizontal Attempt retry inside its own graph,
- vertical child graph creation when later phases enable it.

The orchestrator object is in-process and ephemeral. Durable state lives on
`HarnessGraph`, `Attempt`, tasks, and task outputs.

## Orchestrator responsibilities

For a non-root graph, the local orchestrator:

1. Creates the first Attempt.
2. Spawns the Attempt planner.
3. Materializes the DAG after a valid plan submission.
4. Spawns generator tasks.
5. Watches generator terminal transitions.
6. Spawns the evaluator only after all generators pass.
7. Marks the Attempt passed or failed.
8. Spawns the next Attempt when retry budget remains.
9. Closes the graph when retry is exhausted or a full plan passes.

For `G_root`, the local orchestrator only:

1. Spawns the root executor.
2. Handles root executor terminal submissions.
3. Spawns `REQUEST_PLAN` children in Phase 04.
4. Closes the session graph.

## Attempt stages

| Stage | Running work | Exit condition |
| ----- | ------------ | -------------- |
| `planning` | planner task | planner submits valid plan, or planner run ends without valid submission |
| `generating` | executor and verifier generator tasks | all generators are terminal |
| `evaluating` | evaluator task | evaluator submits success or failure |
| `closed` | none | Attempt is passed or failed |

`REQUEST_PLAN` may pause one executor while its child graph runs. That pause
is not a separate Attempt stage; it is executor-local waiting inside the
`generating` stage.

## Failure escape valves

```
Failure escape valves:
  - Tool-call-level error from any agent
      prehook or handler returns ToolResult(is_error=True)
      -> agent retries inside its own run
      -> no Attempt-level escalation

  - Generator or verifier submit_*_failure
      -> wait for generator quiescence
      -> mark Attempt failed with generator_failed

  - Evaluator submit_evaluation_failure
      -> mark Attempt failed with evaluator_failed immediately

  - Planner agent ends without a successful submit_*_plan
      -> runtime marks Attempt failed with planner_step_budget_exhausted
```

The planner has no failure terminal. Its only soft-fail channel is inline
tool-call rejection. Only a planner run ending without a valid plan submission
escalates to the orchestrator as `planner_step_budget_exhausted`.

## Orchestrator-visible Attempt failures

| Failure mode | Detected by | Wait point |
| ------------ | ----------- | ---------- |
| `planner_step_budget_exhausted` | runtime ends planner without valid plan submission | immediate |
| `generator_failed` | executor or verifier submitted failure | wait until every generator is `DONE`, `FAILED`, or `BLOCKED` |
| `evaluator_failed` | evaluator submitted `submit_evaluation_failure` | immediate |

### Generator-failure quiescence

- When a generator fails, its dependents transition to `BLOCKED`.
- Independent sibling generators keep running.
- The orchestrator does not retry mid-flight.
- After all generators are in `DONE`, `FAILED`, or `BLOCKED`, the
  orchestrator makes one Attempt-level decision.
- The next Attempt's planner receives the whole failure landscape through the
  context engine.

### Evaluator failure

The evaluator is spawned only after every generator is `DONE`, so quiescence
is already satisfied. Evaluator failure triggers Attempt failure immediately.

## Next-Attempt decision

```
try_spawn_next_attempt(G, A_failed, fail_reason):
    A_failed.status      = failed
    A_failed.fail_reason = fail_reason
    A_failed.stage       = closed

    if G.attempts_used < G.retry_budget:
        A_next = G.create_attempt(
            attempt_index    = G.attempts_used + 1,
            prior_attempt_id = A_failed.id,
        )
        G.current_attempt_id = A_next.id
        spawn planner for A_next
        return

    close_harness_graph_failed(
        G,
        source_task_id = A_failed.evaluator_task_id
                      or A_failed.last_failed_generator_task_id
                      or A_failed.planner_task_id,
    )
```

Retry creates a new Attempt in the same graph. It never creates a child graph
and never bubbles up until the graph closes.

## Closure decision tree

### Stage 1: decide current Attempt outcome

```
Orchestrator_G observes a terminal transition in current Attempt A
        |
        v
   A.stage:
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
A failed     no        yes    A passed A failed
(planner_    |          |             (evaluator_failed)
 step_...)   v          v
          keep       any FAILED
          running    or BLOCKED?
                     |
                +----+----+
                v         v
             A failed  spawn evaluator
             (generator_failed)
```

### Stage 2: react at graph level

```
A.status:
  passed
    if A.plan_shape == partial:
      Phase 04 handles CONTINUE_AFTER_PARTIAL_PLAN child spawning.
    else:
      close G success and bubble up to parent.

  failed
    if G.attempts_used < G.retry_budget:
      spawn Attempt N+1 in G.
    else:
      close G failed and bubble up to parent.
```

## Implementation tasks

1. Add local orchestrator lookup by `HarnessGraph.id`.
2. Route terminal handlers through the graph's local orchestrator.
3. Implement planner success path: valid plan submission creates DAG edges
   and generator tasks for the current Attempt.
4. Implement planner exhaustion path.
5. Implement generator failure quiescence and dependent blocking.
6. Implement evaluator spawn after generator success.
7. Implement evaluator success and failure handling.
8. Implement next-Attempt retry inside the same graph.
9. Keep vertical child graph spawning stubbed or feature-gated until Phase 04.

## Phase exit criteria

- A non-root graph can complete a full-plan Attempt successfully.
- Generator failure waits for quiescence before retry.
- Evaluator failure retries immediately when budget remains.
- Planner exhaustion retries or closes according to graph retry budget.
- No retry path creates a child graph.
