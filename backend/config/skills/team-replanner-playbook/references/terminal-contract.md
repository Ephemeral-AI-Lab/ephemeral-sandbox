# Replanner Terminal Contract

Load this reference before drafting any `submit_replan(...)` payload.

## Call Shape

```ts
submit_replan({ new_tasks: NewTaskDefinition[], cancel_ids: string[] })
```

```ts
type TaskSpec = {
  goal: string;
  detail: string;
  acceptance_criteria: string;
};

type NewTaskDefinition = {
  id: string;
  name: "developer" | "validator";
  spec: TaskSpec;
  deps: string[];
  scope_paths: string[];
};
```

Top-level input has only required `new_tasks` and required `cancel_ids`; include `cancel_ids: []` when no cancellation is needed. `new_tasks` must contain at least one corrective task; empty or cancel-only replans are rejected.

Never include `output`, `summary`, `background`, `parent_id`, `new_sibling_tasks`, `new_children_tasks`, `expected_projection`, or prose outside the terminal call.

## Field Rules

| Field | Rule |
| --- | --- |
| `id` | Unique lower-kebab id in this payload. Local deps reference this exact string. |
| `name` | Use only `developer` or terminal `validator`. Never use `team_planner`, `root_planner`, `scout`, `team_replanner`, or any other role. |
| `spec` | Required object with non-empty `goal`, `detail`, and `acceptance_criteria` strings. If `detail` uses `Classification: unresolved_blocker`, it must also include the exact field `Diagnostics decision: trivial_direct_replan` or `Diagnostics decision: deep_diagnostics`. |
| `deps` | Prefer local payload ids. Existing ids require fresh graph proof that they are schedulable and not downstream of this replanner or the failed task. Validators depend on local payload ids. |
| `scope_paths` | Non-empty repo-relative production paths. Verification-only tests stay in `spec`; do not scope benchmark tests, test rewrites, skip/xfail work, pytest config, or transient artifacts unless the original user request asked to repair tests. |
| spec/scope consistency | Trigger -> a spec asks to edit, restore, checkout, or prove no diff for test, benchmark, pytest/config, or verification files; required action -> reject the task unless the original user asked to repair tests and create a production repair or diagnostic instead; failure signal -> production `scope_paths` paired with test-evidence mutation instructions. |

`cancel_ids` may include only stale non-terminal direct siblings of this replanner. Never include the failed task id, original `request_replan` task, this replanner id, terminal tasks, or nested descendants. Same-parent graph position does not make the failed task cancellable. Compare every `cancel_ids` entry against the failed task id from the prompt before submission. Cancel only the stale sibling root; cascade handles descendants and dependents.

Replacement tasks may include a sibling's scope only when that sibling id appears in `cancel_ids`.

## Spec Rules

`spec.goal` names the concrete repair or verification outcome.

`spec.detail` must name classification, diagnostics decision for `unresolved_blocker`, root-cause mechanism or unresolved trace gap, production scope, sibling/cancel handling, dependency context, and evidence/uncertainty.

Same-owner-file repairs are not scope expansion. If the repair remains under any failed-task `scope_paths` entry, use `Classification: unresolved_blocker` with the appropriate diagnostics decision.

Every named failing variant from the failed task summary must be represented in a repair or diagnostic task, or in an explicitly identified live repair owner whose task details or terminal summary covers that same variant and production seam. A preserved downstream validator may verify coverage, but it is not a substitute for repair ownership. Do not bury a named variant only in residual-risk text, "out of scope" text, unsupported/test-design prose, broad validator coverage, or a validator with no upstream repair.

If the failed task proposes a concrete rule or one-line fix, the replan must verify that rule against every observed expected/actual row from the same failing assertion. A rule that fixes one value while breaking another is not a direct repair; create a diagnostic developer to derive the correct production rule.

`spec.acceptance_criteria` must name concrete verification commands or pytest ids and require reporting command output, exit codes, changed behavior, and residual risk.

Acceptance criteria must not use `-k`, parametrization filters, or prose like "do not treat this as a repair target" to avoid a named failing fail-to-pass variant. If a command is narrowed for speed, another local task, preserved validator, or residual risk line must still own each omitted failing variant as production evidence.

Acceptance criteria must not be satisfied by documenting that a fail-to-pass command is expected to fail. Developers change or diagnose production behavior; validators verify repairs and must depend on upstream local repair ids.

## Examples

Add-only direct repair:

```json
{
  "new_tasks": [
    {
      "id": "repair-config-path",
      "name": "developer",
      "spec": {
        "goal": "Repair the config regression in the production loader path.",
        "detail": "Classification: scope_expansion. The failed task proved the root cause is pkg/config.py, not the original assigned file. Run ci_diagnostics(file_path=\"pkg/config.py\") first and preserve named failing test evidence; test paths remain acceptance-only.",
        "acceptance_criteria": "Run uv run pytest tests/test_config.py -q and the focused failing test id; report commands, exit codes, changed behavior, and residual risk."
      },
      "deps": [],
      "scope_paths": ["pkg/config.py"]
    }
  ],
  "cancel_ids": []
}
```

Cancel-and-redraft uses the same shape but sets `cancel_ids` to stale direct sibling ids and scopes replacement work only to those cancelled siblings. Diagnostic replans use `Classification: unresolved_blocker. Diagnostics decision: deep_diagnostics.` in `spec.detail` and may add a validator that depends on local repair ids.

## Final Checklist

| # | Check |
|---|---|
| 1 | Top-level input has only required `new_tasks` and required `cancel_ids`, with `cancel_ids: []` when no sibling should be cancelled. |
| 2 | `new_tasks` contains at least one corrective task; if no task is justified yet, look deeper into the issues and come back with a concrete corrective task. |
| 3 | Every task has only `id`, `name`, `spec`, `deps`, and `scope_paths`. |
| 4 | Every `name` is exactly `developer` or `validator`. |
| 5 | Every id is unique. |
| 6 | Every local dep names another task in this payload; any existing dep is freshly proven schedulable and not downstream of this replanner or the failed task. |
| 7 | Every task has non-empty repo-relative production `scope_paths`. |
| 8 | Every spec is an object with non-empty `goal`, `detail`, and `acceptance_criteria`. |
| 9 | Every spec with `Classification: unresolved_blocker` also includes `Diagnostics decision: trivial_direct_replan` or `Diagnostics decision: deep_diagnostics` inside `detail`. |
| 10 | No named fail-to-pass variant is dropped as a test design issue, unsupported parametrization, cross-engine mismatch, or "not a repair target". |
| 11 | No named fail-to-pass variant appears only as residual risk, "out of scope", unsupported/test-design prose, broad validator coverage, or validator-only closure without an upstream repair. |
| 12 | No proposed one-line rule contradicts another observed value in the same failing assertion. |
| 13 | No task has documentation-only or validation-only acceptance criteria for a known red fail-to-pass command. |
| 14 | `cancel_ids` contains only stale non-terminal direct siblings. |
| 15 | No `cancel_ids` entry equals the failed task id from the prompt, even if that task appears as a same-parent sibling in `read_task_graph()`. |
| 16 | No benchmark tests, `*/tests/*`, `test_*.py`, benchmark harness files, pytest configuration, skip/xfail work, or verification rewrites are scoped unless the original user request explicitly asked to repair tests rather than production behavior. |
| 17 | No task spec tells a developer to edit, restore, checkout, or prove no diff for test evidence while hiding it behind production `scope_paths`. |
| 18 | The final assistant action is the `submit_replan(...)` tool call, not prose. |
