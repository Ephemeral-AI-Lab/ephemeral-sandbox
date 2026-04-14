# Action Reference: add_tasks

Use `add_tasks(...)` when the plan structure is sound but more work is needed. Siblings continue running — no interruption.

## When to choose

Must read sibling and descendant notes via `read_notes(scope="siblings")` before choosing this action — confirm the failure is truly isolated.

- The failure is isolated to one task. Other sibling subtrees (siblings and their children) are unaffected.
- A transient failure (sandbox timeout, network error, flaky test) where the same goal should be retried with fresh state.
- The task partially succeeded but left follow-up work (e.g. fixed 3 of 4 files, need one more task for the last).
- A missing dependency was discovered — add the dependency task, then re-attempt the original goal.

## Retry-as-new-task

When the failure is transient and the scope is correct, create one new task that re-states the original goal. This replaces the old `request_retry` tool. The new task must:

1. Include the original goal verbatim so the agent knows what to do.
2. Append failure context: what went wrong, which error, which attempt number.
3. Add any new `deps` discovered during the failure.
4. Adjust `scope_paths` if the failure revealed the correct owner surface.

Example:
```json
{
  "add_tasks": [
    {
      "id": "fix-compat-retry-1",
      "task": "Fix missing _compat import in pkg/utils.py. Previous attempt failed with sandbox timeout after 30s. Retry with smaller test scope: run pytest pkg/tests/test_utils.py::test_compat_import -x -q first.",
      "agent": "developer",
      "deps": [],
      "scope_paths": ["pkg/utils.py", "pkg/_compat.py"]
    }
  ]
}
```

## Follow-up work pattern

When the original task made progress but left gaps, add targeted follow-up tasks — not a full redo.

Example:
```json
{
  "add_tasks": [
    {
      "id": "fix-remaining-import",
      "task": "Previous task fixed 3/4 import sites. Remaining: pkg/io.py line 42 still imports deprecated `_old_compat`. Change to `_compat`. Verify with pytest pkg/tests/test_io.py -x -q.",
      "agent": "developer",
      "deps": ["fix-compat-v1"],
      "scope_paths": ["pkg/io.py"]
    }
  ]
}
```

## Rules

- Must pair each new developer task with a validator task (`cascade_policy: "continue"`).
- Must include the exact failing test ids and error snippet from the validator packet in the new task briefing.
- Must include failure context from the previous attempt so the agent does not repeat the same approach.
- Never bundle unrelated fixes into one task.
- Never omit `scope_paths` — the new task must declare its ownership surface.
