# Terminal Validation Contract

Use this reference when shaping the terminal validator task in a plan.

## Task/Goal

- You are writing the single terminal validator task for a submitted plan.

## Avoid

- The terminal validator's top-level `deps` field must list every terminal non-validator sibling id.
- Do not rely on validator prose like "depends on every sibling"; prose inside `spec` does not set task dependencies.
- Never submit a `validator` task with `deps: []` when the plan has non-validator siblings.
- The terminal validator must still run even when some deps fail.
- The task prose must not limit verification to only the scoped tests — it must run the full suite to catch cross-scope regressions like broken imports in shared files.
- The task prose must instruct the validator to run `ci_diagnostics` as a pre-flight check before the full suite.

## Workflow

- The terminal validator must run the **entire relevant test suite**, not just the scoped tests from individual developer lanes. Its task prose must specify the broad verification command that covers all fail-to-pass and pass-to-pass targets.
- The terminal validator's `spec` field must include:

1. **Full suite command**: the broad pytest or test command covering all targets from the original benchmark or user request.
2. **Scoped re-check list**: the specific failing test ids from developer lanes, so the validator can attribute regressions to specific changes.
3. **Diagnostic pre-check**: instruct the validator to run `ci_diagnostics(file_path)` on every `scope_paths` file before the full suite, catching import and name errors early.
4. **Tool deps**: the validator task item's `deps` array must contain the sibling task ids it validates.

## Expected Outcome

- The plan ends with one terminal validator that can catch both direct regressions and cross-scope breakage.
