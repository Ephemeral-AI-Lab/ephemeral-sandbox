# Action Reference: submit_replan (cancel and redraft)

Use this reference for `submit_replan(new_tasks=[...], cancel_ids=[...])` only when one or more stale non-terminal direct siblings other than the original failed `request_replan` task must be replaced. Cancelling a sibling cascades to its subtree automatically; replacements go in `new_tasks` as direct children of this replanner.

If the only obsolete task is the original failed `request_replan` task, do not use this action. Use `action-add-tasks` and submit `cancel_ids=[]`; the runtime finalizes the failed task after a valid replan.

## Task/Goal

- A live direct sibling other than the original failed task is working on invalidated assumptions, a shared dependency changed, or adding corrective tasks alone would leave stale work running.

## Avoid

- Never cancel DONE, FAILED, or CANCELLED tasks; terminal records are immutable.
- Never cancel the original failed `request_replan` task; it is immutable failure evidence.
- Never try to cancel a non-sibling (a nested task inside a sibling's subtree). Cancel the sibling root and let cascade handle the subtree.
- Do not cancel tasks without confirming they are actually stale.
- Do not repair an uncancelled sibling's scope inside a replacement task. Cancel stale siblings first, or leave valid sibling ownership alone.
- Do not replace a failed task with a benchmark-test edit task because the failure packet suggests the test is wrong; use a live production boundary or `team_planner` scoped to the nearest boundary instead of a test-edit developer task.
- Do not use a replacement that creates, renames, moves, or re-exports a test-derived missing path whose only evidence is a test import or collection error. A similar in-scope compatibility filename is not an exception.

## Workflow

- `cancel_ids` accepts only non-terminal direct siblings of this replanner (same `parent_id`) after excluding the `Failed task id`. Use same-parent peer context for cancel candidates; do not promote ids from global or nested graph rows.
- If excluding the `Failed task id` leaves no stale sibling to cancel, switch to `action-add-tasks` and submit `cancel_ids=[]`.
- Replacement work belongs in `new_tasks`. If the replacement needs a hierarchy, make it a `team_planner` task (its terminal is `submit_plan`, not `submit_replan`).
- Replacement `scope_paths` may include a sibling's owner path only when that sibling id is in `cancel_ids`; otherwise the sibling still owns that work.
- If the missing import path is named only by tests and no non-test production owner was proven, do not replace with a missing-path task; use `submit_replan(new_tasks=[], cancel_ids=[])` unless a stale sibling must still be cancelled for another reason.
- For replacement file moves, renames, shims, and re-export bridges, verify both source and destination ownership; do not write an absent outside-scope destination named only by tests, even when the source file is in scope.
- Replacement specs relocating or renaming a path must name `daytona_move_file`. Pure removals may run through CodeAct or `daytona_delete_file`. Do not tell children to bypass coordinated tools with standard Python file I/O, CodeAct writes, shell redirects, or whole-file overwrite fallback instructions.
- Replacement tasks must not depend on downstream tasks already blocked on this replanner (cycle).
- Prefer `deps` ids local to this payload; validator deps must be local. Existing-task deps must be freshly proven schedulable and not downstream of this replanner or the original failed task.
- If `new_tasks` has 3+ concrete non-planner replacements, add one terminal `validator` whose `deps` cover them.
- Replacement `scope_paths` must be repo-relative with no `/testbed/...` prefixes, and specs must not say `cd /testbed`, "run from /testbed", or add `2>&1`, output redirects, `| head`, or `| tail`; CodeAct starts at repo root and captures output automatically.
- Each replacement `spec` uses numbered colon labels in exact order: `1. Goal:`, `2. Environment:`, `3. Scope:`, `4. Context:`, `5. Acceptance Criteria:`. Each label starts its own line and has body text on that same line. Do not put all labels on one line. Do not put the body on the next line after the colon. Do not use Markdown headings. Do not include `task_note`, `output`, `summary`, `background`, `parent_id`, or any top-level field besides `new_tasks` and `cancel_ids`. The system generates the outcome summary automatically once the corrective children complete.
- Self-check that `cancel_ids` excludes the literal `Failed task id` from the header, the original failed task, and terminal siblings; also verify no replacement scopes benchmark tests unless the prompt explicitly owns a test-only bug.
- Self-check the final payload before the single terminal call. If `submit_replan(...)` is rejected, do not call CI, file, graph, note, or CodeAct tools; retry only a mechanical correction from the validation message.

## Expected Outcome

- Stale sibling work is replaced cleanly at this layer without duplicate or dangling work; deeper subtrees are cleaned up by cascade.
