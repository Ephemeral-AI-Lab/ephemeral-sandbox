# Action Reference: Cancel And Redraft

Use this after failure-mode classification and any diagnostics have produced a corrective mapping that requires replacing stale same-layer work. Final payload shape lives in `terminal-contract`; this reference only decides cancellation boundaries.

If no stale direct sibling remains after excluding the failed task, switch to `action-add-tasks` and submit `cancel_ids=[]`.

## Allow

- Cancel a non-terminal direct sibling of this replanner when it is stale because:
  - it is working from invalidated assumptions
  - a shared dependency changed
  - leaving it running would duplicate or conflict with replanner-owned repair
- Put replacement work in `new_tasks` as direct children of this replanner.
- Include a cancelled sibling's scope in replacement tasks only when that sibling id is in `cancel_ids`.

## Never Cancel

- The failed task or original `request_replan` task.
- This replanner.
- `done`, `failed`, or `cancelled` tasks.
- Nested descendants or dependents directly; cancel the stale sibling root and let cascade handle them.
- Tasks that are merely inconvenient but not stale.

## Drop

- Same-scope continuation of the failed task.
- Benchmark-test edits or missing paths proven only by tests.
- Replacement work for an uncancelled sibling's scope.
- Replacement moves, shims, bridges, or re-exports without production evidence for both source and destination.
- Dependencies on downstream tasks already blocked on this replanner.

## Build

1. Confirm each `cancel_ids` item is a non-terminal direct sibling with the same `parent_id` as this replanner.
2. Exclude the failed task id, this replanner id, terminal tasks, and nested graph ids.
3. Keep replacement work under this replanner; do not create a child `team_planner` or `team_replanner` to decide the repair.
4. Prefer local deps; existing deps require fresh graph proof that they are schedulable and not downstream of this replanner or the failed task.
5. If `new_tasks` has 3 or more concrete non-planner replacements and no preserved downstream validator covers the surface, add one terminal validator whose `deps` cover them. This matches the shared Terminal Validator Rule in `terminal-contract`.
6. Load `terminal-contract`, self-check the payload, then submit exactly one `submit_replan(...)` call.

## Expected Outcome

Stale sibling work is replaced cleanly at this layer; cascade handles deeper cleanup without duplicate or dangling work.
