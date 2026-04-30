# Phase 00 - Target Architecture

## Goal

Define the target harness model before changing implementation details.
The new architecture separates graph depth from retry history:

- A vertical axis of nested `HarnessGraph`s for real scope changes.
- A horizontal axis of `Attempt`s inside each graph for retry.
- One in-process local orchestrator per `HarnessGraph`.

## Target model

The harness is a tree of `HarnessGraph`s. Each graph owns a scoped goal and
an ephemeral local orchestrator. Inside each non-root graph, the orchestrator
runs ordered `Attempt`s: fresh `planner -> DAG -> evaluator` passes until
one passes or the graph retry budget is exhausted.

```
USER QUERY
    |
    v
HARNESS GRAPH G_root
  - no parent
  - bound to the session
  - contains only the root generator/executor
  - retry_budget = 0
    |
    | root executor calls submit_request_plan(note)
    v
HARNESS GRAPH G1 (spawn_reason = REQUEST_PLAN)
  - retry_budget = N
  - Attempt 1: planner -> DAG -> evaluator, failed
  - Attempt 2: planner -> DAG -> evaluator, failed
  - Attempt 3: planner -> DAG -> evaluator, passed
    |
    | if final plan_shape == partial
    v
HARNESS GRAPH G_next (spawn_reason = CONTINUE_AFTER_PARTIAL_PLAN)
  - its own retry budget
  - its own Attempts
```

Explorer subagents are not in `TaskCenter`; they are non-blocking,
parallel-safe helper runs. Advisor and resolver helper calls are also not
TaskCenter graph nodes. Advisor is read-only. Resolver is blocking and may
edit, but it reports back into the task that called it.

## Two axes of progression

| Axis       | Entity         | What changes          | Triggered by                                  | Tree shape effect                              |
| ---------- | -------------- | --------------------- | --------------------------------------------- | ---------------------------------------------- |
| Vertical   | `HarnessGraph` | new scope             | `REQUEST_PLAN`, `CONTINUE_AFTER_PARTIAL_PLAN` | graph depth increases                          |
| Horizontal | `Attempt`      | same scope, fresh try | Attempt failure under retry budget            | no new graph depth; retry stays inside graph   |

### Vertical axis

A `HarnessGraph` represents a scoped goal. New graphs are created only when
the pipeline needs new scoped work:

- `REQUEST_PLAN`: an executor delegates a subgoal. The executor's enclosing
  graph spawns a child graph for that subgoal.
- `CONTINUE_AFTER_PARTIAL_PLAN`: a graph completed a partial plan and left
  forward instructions. The completing graph spawns a child graph to continue.

Both cases deepen the graph tree.

### Horizontal axis

An `Attempt` is one full `planner -> DAG -> evaluator` pass at the graph's
goal. A graph holds an ordered list of Attempts; only the latest Attempt is
running. When an Attempt fails and retry budget remains, the orchestrator
spawns the next Attempt inside the same graph.

Retry never deepens the graph tree.

## Why the split matters

- Tree depth reflects real pipeline depth, not retry count.
- `prior_graph_id` is reserved for partial-plan segment chains.
- Retry history lives on `Attempt` rows inside a graph.
- Retry budget is graph-local.
- Bubble-up only happens when a child graph closes, never during retry.
- From a parent graph's perspective, each child graph has one final outcome.

## Components

| Component          | Owner / scope                     | Responsibility |
| ------------------ | --------------------------------- | -------------- |
| `HarnessGraph`     | `TaskCenter`                      | Container for one scoped goal. Holds lineage, spawn reason, retry budget, and ordered Attempts. |
| `Attempt`          | `TaskCenter`, owned by a graph    | One pass at the graph goal: planner, DAG edges, generator task ids, evaluator, status, failure reason. |
| `Orchestrator`     | one per `HarnessGraph`            | Drives local graph lifecycle, creates Attempts, observes terminals, spawns child graphs, closes the graph. |
| `RootHarnessGraph` | `TaskCenter` singleton/session    | Wraps the root executor only. No Attempts. Closing it ends the session. |
| Tasks              | per `Attempt`                     | Agent runs scoped to one Attempt of one graph. |

## Root graph rules

- `G_root` exists for the lifetime of the session.
- It contains only the root generator/executor.
- It has no Attempt list because the root executor is not a
  `planner -> DAG -> evaluator` pass.
- Its orchestrator initializes the root executor.
- It catches root executor terminals:
  - `submit_request_plan(note)` spawns a `REQUEST_PLAN` child graph and later
    delivers the child close report back to the root executor.
  - `submit_execution_success` and `submit_execution_failure` close `G_root`.
- The root executor uses the same executor tool-gating rules as any other
  executor.
- `G_root.retry_budget = 0`.

## Phase exit criteria

- The team agrees that retry is horizontal and graph-local.
- The team agrees that `REQUEST_PLAN` and `CONTINUE_AFTER_PARTIAL_PLAN` are
  the only vertical spawn reasons.
- The team agrees that `RETRY_ON_FAILURE` is not a graph spawn reason.
- The context-engine boundary is explicit: planner launch context, evidence
  storage, and detailed close-report payloads are specified separately.
