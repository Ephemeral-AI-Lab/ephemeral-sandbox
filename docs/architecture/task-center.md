# Task Center

TaskCenter owns the team task graph, notes, status transitions, budget counters, task context assembly, and replan application. It is the coordination layer between planners, workers, replanners, and the dispatch queue.

## Responsibilities

- Insert validated plans into the task DAG.
- Track task status and dependency readiness.
- Build injected task context through `TaskContextBuilder` from dependency notes, parent context, replanner root cause traces, and recent scope changes.
- Mark work complete or failed.
- Spawn replanner tasks when a worker submits failure.
- Apply replanner output by inserting new tasks, cancelling stale tasks, and completing or expanding the replanner.

## Statuses

Task statuses are `pending`, `ready`, `running`, `expanded`,
`expanded_awaiting_summary`, `request_replan`, `done`, `failed`, and
`cancelled`.

`done`, `failed`, `cancelled`, and `request_replan` are terminal.

## Replanning

When a worker reports failure, the executor calls `TaskCenter.request_replan`.
The executor only interprets the agent result; TaskCenter owns the lifecycle
mutation, budget accounting, event emission, and persistence transaction.

TaskCenter marks the original task `request_replan`, creates a replanner task, and
rewires each `pending` dependent from the failed task to the replanner. A
dependent with any other status is a task graph invariant violation: downstream
work that depends on the failed task should not be `ready`, `running`,
`expanded`, `request_replan`, or terminal.

`GraphInvariantViolation` is fatal to the team run. The executor immediately
fails the team run so the corrupted task graph cannot continue dispatching.

Dependency readiness is strict: a task can leave `pending` for scheduler-owned
work states (`ready`, `running`, `expanded`, `request_replan`, or `done`) only when
all dependency tasks are `done`. Failed, cancelled, missing, `request_replan`,
expanded, running, ready, or pending dependencies are unsatisfied.

The replanner submits `submit_replan(new_tasks=[...], cancel_ids=[...])`.

After the replan:

- `new_tasks` are inserted as direct children of the replanner at the replanner's depth. The replanner never sets `parent_id` per task.
- Each `new_tasks` item carries the full task briefing in `spec`; a separate short `description` label is not required.
- The full corrective task JSON is appended to the replanner detail as `Initial Replan`; the replanner does not submit a free-text summary.
- `cancel_ids` may target only direct siblings of the replanner. Cancelled tasks are marked `cancelled`, including cascaded descendants and dependents.
- New replan tasks may depend on local new-task IDs or schedulable existing tasks (`done`, `ready`, `pending`) that do not already depend on the replanner or the original failed task.
- The replanner is marked `done` immediately when it has no new child tasks, or `expanded` when it created direct child tasks.
- Expanded replanners transition to `expanded_awaiting_summary` after all direct children are terminal; `parent_summarizer` then reads every child detail, posts the roll-up, and finalizes the replanner as `done` only when the roll-up has no unresolved child evidence.
- A `parent_summarizer` may call `request_replan(reason=...)` for unresolved roll-ups; the executor targets that replan at the summarized parent.
- The original failed task stays `request_replan` after the replanner succeeds. The origin is terminal from recovery start; success records `replanned_by:<replanner_id>` on its failure reason while pending dependents remain rewired to the replanner.

## Notes

Notes are scoped by task and path. `NoteManager` owns note state, posting, reads, and scope filtering. `TaskContextBuilder` owns agent-facing context injection: the assigned task, dependency notes, parent context, replanner root cause traces, and recent overlapping scope changes.

When multiple notes exist for the same upstream task, prompt context prefers the
most useful note over the merely latest note. For dependency context,
`TaskContextBuilder` keeps one preferred note per dependency task, avoiding
low-information status notes when richer worker, scout, planner, or reviewer
notes exist. For parent context, it first prefers notes whose paths match the
child task's `scope_paths`, then applies the same preferred-note selection.

## Resume

TaskCenter no longer exposes a user-facing checkpoint or rollback API. Crash
recovery rebuilds the task graph from the event log, primes resume-only state,
and `prepare_for_resume()` restores the replayed task snapshot into the store
before recovering `running` tasks back to `ready`.
