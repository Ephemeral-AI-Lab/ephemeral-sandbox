# Action Reference: cancel_and_redraft

Use `cancel_and_redraft(...)` when the current subtree decomposition is fundamentally wrong. Cancel stale sibling work and replace it with a corrected plan.

## When to choose

- The original decomposition targeted the wrong files, wrong ordering, or wrong approach.
- Multiple siblings are failing for different reasons that all trace back to a flawed plan structure.
- The scope boundaries between tasks were drawn incorrectly (overlapping ownership, missing surfaces).
- A completed task's changes invalidated the premise of remaining siblings — the plan needs restructuring, not patching.

## Signals that point to cancel_and_redraft over add_tasks

"Siblings" here means sibling tasks **and their descendant subtrees**.

- More than half of the sibling subtrees have failed or will fail given the current plan.
- The failure reason is "wrong owner file" or "wrong decomposition" rather than a bug or transient error.
- Adding corrective tasks would create a tangled dependency graph — starting fresh is cleaner.
- The validator packet reveals that the original plan's assumption about the codebase structure was incorrect.
- Must read sibling and descendant notes via `read_notes(scope="siblings")` before concluding the decomposition is wrong.

## Arguments

```json
{
  "add_tasks": [
    {
      "id": "rewrite-compat-v2",
      "task": "Original plan split compat work by file, but the compat surface is a single module (pkg/_compat.py) exporting to 4 consumers. Fix the export surface in pkg/_compat.py, then verify all 4 consumers import correctly.",
      "agent": "developer",
      "deps": [],
      "scope_paths": ["pkg/_compat.py"]
    },
    {
      "id": "verify-compat-v2",
      "task": "Verify all 4 consumer imports resolve after the compat fix. Run pytest pkg/tests/ -x -q.",
      "agent": "developer",
      "deps": ["rewrite-compat-v2"],
      "scope_paths": ["pkg/tests/"],
      "cascade_policy": "continue"
    }
  ],
  "cancel_ids": ["fix-io-v1", "fix-parser-v1", "fix-cli-v1", "verify-io", "verify-parser", "verify-cli"]
}
```

- `add_tasks`: The replacement plan. Must be self-contained — cannot depend on cancelled tasks.
- `cancel_ids`: IDs of sibling tasks to cancel. Include both developer and validator tasks that are stale.

## Rules

- Must only cancel tasks that are genuinely stale. Never cancel a sibling that completed successfully with valid work.
- Must ensure replacement tasks do not depend on any cancelled task.
- Must include failure context from the original plan in new task briefings so the agent does not repeat the same mistakes.
- Must pair each developer task with a validator task (`cascade_policy: "continue"`).
- Must call `context_changed_since()` before submitting if freshness moved.
- Never use cancel_and_redraft when fewer than half of siblings are affected — use `add_tasks` instead.
