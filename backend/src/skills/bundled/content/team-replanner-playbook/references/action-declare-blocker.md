# Action Reference: declare_blocker

Use `declare_blocker(...)` when a shared dependency is broken and multiple siblings will hit the same error. The conductor pauses affected work, spawns a single resolver, and resumes everything after the fix.

## When to choose

- Sibling notes show the same file or symbol causing failures across multiple tasks.
- A completed sibling broke a shared import, config, or schema that others depend on.
- The failure is not in the failed task's own scope — it's in shared infrastructure.
- Fixing the issue in each sibling independently would be redundant and wasteful.

## Signals that point to declare_blocker over add_tasks

"Siblings" here means sibling tasks **and their descendant subtrees** — a child task's failure counts as a sibling-scope signal.

- Two or more sibling subtrees (notes from any depth) mention the same file path in their error.
- The error is an ImportError, ModuleNotFoundError, or schema validation error on a shared module.
- The failing task's `scope_paths` does not include the broken file — it's outside its ownership.
- `read_notes(scope="siblings")` shows a pattern of the same root cause across subtrees. Must read notes before concluding a failure is isolated.

## Arguments

```
declare_blocker(
    root_cause_paths=["pkg/shared_config.py"],
    reason="pkg/shared_config.py was refactored by task fix-config-v1, removing the `load_defaults()` export. Tasks fix-io, fix-parser, and fix-cli all import it.",
    suggestion="Restore `load_defaults` export or add a backward-compat alias in pkg/shared_config.py"
)
```

- `root_cause_paths`: The exact broken shared files. Must be confirmed live with CI tools.
- `reason`: Why this is a shared blocker, not an isolated failure. Name the affected siblings.
- `suggestion`: Optional hint for the resolver about the expected fix direction.

## Rules

- Must confirm root cause paths are live with CI tools before declaring.
- Must name specific affected siblings in the reason — vague "multiple tasks affected" is not enough.
- Must only declare a blocker when ≥2 siblings are affected or will be affected.
- Never declare a blocker when only the failed task is affected — use `add_tasks` instead.
- Must call `context_changed_since()` before submitting if freshness moved.
