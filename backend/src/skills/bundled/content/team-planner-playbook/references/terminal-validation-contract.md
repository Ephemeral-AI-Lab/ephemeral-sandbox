# Terminal Validation Contract

Use this reference when shaping the terminal validator task in a plan.

## Full-suite requirement

The terminal validator must run the **entire relevant test suite**, not just the scoped tests from individual developer lanes. Its task prose must specify the broad verification command that covers all fail-to-pass and pass-to-pass targets.

## Task prose template

The terminal validator's `objective` field must include:

1. **Full suite command**: the broad pytest or test command covering all targets from the original benchmark or user request.
2. **Scoped re-check list**: the specific failing test ids from developer lanes, so the validator can attribute regressions to specific changes.
3. **Diagnostic pre-check**: instruct the validator to run `ci_diagnostics(file_path)` on every `scope_paths` file before the full suite, catching import and name errors early.

## Example

```json
{
  "id": "val-terminal",
  "agent": "validator",
  "deps": ["dev-a", "dev-b", "plan-c"],
  "scope_paths": ["pkg/io/", "pkg/repo/"],
  "cascade_policy": "continue",
  "objective": "Terminal validation gate. (1) Run ci_diagnostics on each scope_paths entry to catch import/name errors early. (2) Run the full test suite: shell('python -m pytest tests/ -x --timeout=300'). (3) Report exact failing ids, exit codes, and error snippets. If any developer lane introduced regressions outside its own scope, include that in the failure evidence."
}
```

## Rules

- The terminal validator must depend on every terminal non-validator sibling (already enforced by plan-json-contract).
- The terminal validator's `cascade_policy` must be `"continue"` so it runs even when some deps fail.
- The task prose must not limit verification to only the scoped tests — it must run the full suite to catch cross-scope regressions like broken imports in shared files.
- The task prose must instruct the validator to run `ci_diagnostics` as a pre-flight check before the full suite.
