# Task Center

TaskCenter is the composition facade for team coordination: task graph
persistence, notes, budgets, context assembly, event emission, and runtime
wiring. Runtime status transitions flow through `TaskCoordinator`, while
`TaskQueue` dispatches ready task ids to the executor.

## Responsibilities

- Insert validated plans into the task DAG.
- Track task status and dependency readiness.
- Build injected task context through `TaskContextBuilder` from the assigned task, replanner root cause traces, and recent scope changes.
- Route work completion, failure, cancellation, parent roll-up synthesis, and replan requests through `TaskCoordinator`.
- Spawn replanner tasks when a worker submits `request_replan`.
- Apply replanner output by inserting new tasks, cancelling stale tasks, and completing or expanding the replanner through the unified coordinator.

## Statuses

Task statuses are `pending`, `ready`, `running`, `expanded`,
`request_replan`, `done`, `failed`, and `cancelled`.

`done`, `failed`, `cancelled`, and `request_replan` are terminal.

## Replanning

When a worker reports failure, the executor returns
`TaskStatusUpdate(REQUEST_REPLAN, summary=...)`. The executor only interprets
terminal tool metadata; `TaskCoordinator` owns the lifecycle mutation, budget
accounting, event emission, and persistence transaction.

`TaskCoordinator` marks the original task `request_replan`, creates a replanner
task, and rewires each `pending` dependent from the failed task to the replanner. A
dependent with any other status is a task graph invariant violation: downstream
work that depends on the failed task should not be `ready`, `running`,
`expanded`, `request_replan`, or terminal.

`GraphInvariantViolation` is fatal to the team run. The failed status update
routes through `TaskCoordinator`, which fail-fasts the run so the corrupted
task graph cannot continue dispatching.

Dependency readiness is strict: a task can leave `pending` for scheduler-owned
work states (`ready`, `running`, `expanded`, `request_replan`, or `done`) only when
all dependency tasks are `done`. Failed, cancelled, missing, `request_replan`,
expanded, running, ready, or pending dependencies are unsatisfied.

The replanner submits `submit_replan(new_tasks=[...], cancel_ids=[...])`.

After the replan:

- `new_tasks` are inserted as direct children of the replanner at the replanner's depth. The replanner never sets `parent_id` per task.
- Each `new_tasks` item carries the full task briefing in `spec`; a separate short `description` label is not required.
- The replanner does not submit a free-text summary.
- `cancel_ids` may target only direct siblings of the replanner. Cancelled tasks are marked `cancelled`, including cascaded descendants and dependents.
- New replan tasks may depend on local new-task IDs or schedulable existing tasks (`done`, `ready`, `pending`) that do not already depend on the replanner or the original failed task.
- A replanner that produces no corrective child tasks is failed as an invalid recovery result.
- Expanded replanners become `done` after all direct children are terminal and `TaskCoordinator` synthesizes the roll-up from child submissions.
- The original failed task stays `request_replan` after the replanner succeeds. The origin is terminal from recovery start; success records `replanned_by:<replanner_id>` on its failure reason while pending dependents remain rewired to the replanner.

## Notes

Notes are file scoped only. `NoteManager` owns append-only note state, posting,
path-based reads, and scope filtering. Notes are not attached to task ids, do
not form parent/child threads, and are not appended to `read_task_details`
output. Agents use `read_file_note(file_path=...)` for file evidence instead of
receiving task-scoped note context automatically.

## Resume

TaskCenter no longer exposes a user-facing checkpoint or rollback API. Crash
recovery rebuilds the task graph from persistence and recovers `running` tasks
back to `ready` using the persistent task graph directly.
