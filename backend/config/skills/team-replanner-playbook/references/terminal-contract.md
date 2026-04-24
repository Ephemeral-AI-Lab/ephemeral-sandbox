# Replanner Terminal Contract

Use while drafting and checking the final `submit_replan(...)` payload.

## Call Shape

```ts
submit_replan({ new_tasks: NewTaskDefinition[], cancel_ids: string[] })
```

```ts
type NewTaskDefinition = {
  id: string;
  agent: "developer" | "validator" | "team_planner";
  spec: {
    goal: string;
    detail: string;
    acceptance_criteria: string;
  };
  deps: string[];
  scope_paths: string[];
};
```

Top-level input has only `new_tasks` and `cancel_ids`; use `cancel_ids: []` when no sibling should be cancelled. `new_tasks` is non-empty.

## Field Rules

| Field | Rule |
| --- | --- |
| `id` | Unique lower-kebab id in this payload. |
| `agent` | `developer`, `validator`, or `team_planner` with Planner handoff. |
| `spec` | Non-empty `goal`, `detail`, and `acceptance_criteria`. |
| `deps` | Prefer local payload ids; existing ids require fresh graph proof that they are schedulable and not downstream of this replanner or the failed task. |
| `scope_paths` | Repo-relative production paths; tests and benchmark harnesses stay in `spec`. |
| `cancel_ids` | Only stale running/pending/ready direct siblings; never failed, `request_replan`, replanner, terminal, descendant, or validator-continuation work. |

`spec.detail` names classification, diagnostics decision for `unresolved_blocker`, Planner handoff for broad `team_planner` redrafts, root-cause mechanism or gap, production scope, original-contract coverage, sibling/cancel handling, dependency context, evidence, and uncertainty.

`spec.acceptance_criteria` names concrete commands or pytest ids and asks for command output, exit codes, changed behavior, and residual risk. Named fail-to-pass variants stay owned by a repair/diagnostic task or preserved live owner; validator-only closure is not enough.

## Final Checklist

| # | Check |
| --- | --- |
| 1 | Top-level input has only `new_tasks` and `cancel_ids`. |
| 2 | `new_tasks` contains at least one corrective task. |
| 3 | Every task has only `id`, `agent`, `spec`, `deps`, and `scope_paths`. |
| 4 | Every `agent` is `developer`, `validator`, or Planner-handoff `team_planner`. |
| 5 | Local deps name another task in this payload; existing deps are freshly proven schedulable. |
| 6 | Every task has non-empty production `scope_paths`. |
| 7 | Every unresolved-blocker spec includes `Diagnostics decision: trivial_direct_replan` or `Diagnostics decision: deep_diagnostics`. |
| 8 | Named fail-to-pass variants and uncompleted original task criteria are not dropped as unsupported, test design, residual risk, or validator-only coverage. |
| 9 | Test/benchmark/pytest-config restore/edit stays evidence; no child task owns it. |
| 10 | `cancel_ids` contains only stale running/pending/ready direct siblings and no failed, terminal, replanner, descendant, `request_replan`, or validator-continuation work. |
| 11 | The final assistant action is the `submit_replan(...)` tool call, not prose. |
