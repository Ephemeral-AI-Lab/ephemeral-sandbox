---
intent: read_only
terminal: true
hooks: [no_background_sessions, advisor_approval]
---
Terminate your reducer run with SUCCESS or FAILED for the current reducer task.

## Inputs
- `status`: `"success"` when the assigned reducer work is complete, or
  `"failed"` when it cannot be completed from the current context.
- `outcome`: 1-3 sentence summary of the completed reducer result or the
  concrete blocker/missing context.

## Behavior
- Records reducer success or failure on this task.
- A successful reducer can close the attempt once every plan task is done.
- A failed reducer causes the attempt lifecycle to fail or replan.

## Success vs Failure Decision

Reducer task:
- Treat `<dependencies>` outcomes as context inputs for your `<assigned_task>`.
- Work on the assigned reducer task, then choose exactly one terminal tool.

Call `submit_reducer_outcome` with `status="success"` when:
- You finished the assigned reducer work.
- Your `outcome` summarizes what you completed and the reducer outcome/context
  that should be carried forward.

Call `submit_reducer_outcome` with `status="failed"` when:
- You cannot finish the assigned reducer work from the current context.
- The dependency outcomes are missing, contradictory, insufficient, or expose a
  blocker that requires another attempt or planner iteration.

Do not submit success just because dependency outcomes look reasonable. Success
means the assigned reducer work is finished; otherwise, submit failure with the
specific blocker or missing context.