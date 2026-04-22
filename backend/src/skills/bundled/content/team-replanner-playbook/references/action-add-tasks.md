# Action Reference: Add Corrective Tasks

Use this after failure-mode classification and any diagnostics have produced a corrective mapping with no stale sibling cancellation. Final payload shape lives in `terminal-contract`; this reference only decides what work is allowed.

If the payload needs any `cancel_ids`, stop and load `action-cancel-and-redraft` instead.

## Allow

- Add direct children of this replanner for:
  - `scope_expansion`
  - `wrong_owner_or_role`
  - `unresolved_blocker` after diagnostics identify a production repair surface
- Keep repair work anchored to the failed task and preserved dependents.
- Merge nearby same-file seams into one developer task.
- Add a terminal validator only when policy/size requires it or no preserved downstream validator covers the repair.

## Drop

- Same-scope continuation of unfinished failed-task work.
- Budget exhaustion, failed attempts, incomplete verification, or ambient sibling drift.
- Benchmark-test edits, test-derived helpers, and missing paths proven only by tests.
- Work already owned by an uncancelled live sibling.
- Duplicate validators/dependents already rewired to this replanner.
- New-file, move, shim, bridge, or re-export work without production evidence for the destination.

## Build

1. For each candidate task, name the failure mode and root-cause trace entry it addresses.
2. Keep `cancel_ids=[]`.
3. Use local deps only for real output ordering; do not add deps for mere scope overlap.
4. Prefer one developer per exact production file unless disjoint edit regions are proven.
5. Tell corrective developers to run `ci_diagnostics(file_path=...)` first.
6. For moves/renames, name `daytona_move_file`; for pure removals, `daytona_delete_file` or CodeAct is acceptable.
7. If `new_tasks` has 3 or more concrete non-planner children and no preserved downstream validator covers the surface, add one terminal validator whose `deps` cover them. This matches the shared Terminal Validator Rule in `terminal-contract`.
8. Load `terminal-contract`, self-check the payload, then submit exactly one `submit_replan(...)` call.

## Expected Outcome

The replanner adds only missing corrective children, leaves valid siblings running, and lets already-rewired downstream tasks wait on this replanner instead of duplicating them.
