# Task Center

TaskCenter owns the team task graph, notes, status transitions, budget counters, and replan application. It is the coordination layer between planners, workers, replanners, and the dispatch queue.

## Responsibilities

- Insert validated plans into the task DAG.
- Track task status and dependency readiness.
- Build task context from dependency notes, sibling notes, parent context, retry state, and recent scope changes.
- Mark work complete or failed.
- Spawn replanner tasks when a worker submits failure.
- Apply replanner output by inserting new tasks, cancelling stale tasks, and completing or expanding the replanner.

## Statuses

Task statuses are `pending`, `ready`, `running`, `expanded`, `replanning`, `done`, `failed`, and `cancelled`.

`done`, `failed`, and `cancelled` are terminal.

## Replanning

When a worker reports failure, the executor calls `TaskCenter.request_replan`.
The executor only interprets the agent result; TaskCenter owns the lifecycle
mutation, budget accounting, event emission, and persistence transaction.

TaskCenter marks the original task `replanning`, creates a replanner task, and
rewires each `pending` dependent from the failed task to the replanner. A
dependent with any other status is a task graph invariant violation: downstream
work that depends on the failed task should not be `ready`, `running`,
`expanded`, `replanning`, or terminal.

`GraphInvariantViolation` is fatal to the team run. The executor does not route
it through the normal worker-error retry path; it immediately fails the team run
so the corrupted task graph cannot continue dispatching.

Dependency readiness is strict: a task can leave `pending` for scheduler-owned
work states (`ready`, `running`, `expanded`, `replanning`, or `done`) only when
all dependency tasks are `done`. Failed, cancelled, missing, replanning,
expanded, running, ready, or pending dependencies are unsatisfied.

The replanner submits `submit_replan(new_tasks=[...], cancel_ids=[...])`.

After the replan:

- New tasks are inserted at each submitted task's explicit `parent_id`; this may be the replanner itself, the replanner's parent, or a surviving task inside that parent projection.
- Cancelled not-completed tasks are marked `cancelled`, including cascaded descendants and dependents.
- The replanner is marked `done` immediately when it has no direct child tasks, or `expanded` when it created direct child tasks.
- Expanded replanners are marked `done` only after all direct children finish successfully.
- The original failed task is marked `failed` without cascading after the replanner succeeds, because pending dependents have already been rewired to the replanner.

## Notes

Notes are scoped by task and path. Task context includes the assigned task, dependency notes, sibling notes, parent context, retry notes, and recent overlapping scope changes.
