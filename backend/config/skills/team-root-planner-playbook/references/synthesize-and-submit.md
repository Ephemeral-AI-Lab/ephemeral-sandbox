# Root Planner Submit Plan Reference

Load this reference in Stage 3 only, after the owner ledger is complete and useful scouts have joined or been skipped.

## Routing Flow

```text
Caption: root planner routes owner-ledger rows without exploring every leaf.

owner slice
  |-- atomic + exact owner + one mechanism --------------> developer
  |-- clustered / matrix / unresolved residue only ------> team_planner
  `-- same-payload verification after producers ---------> validator
```

| Slice signal | Route |
| --- | --- |
| Live evidence names one owner file, symbol, or tight production surface | `developer` |
| Several mechanisms, APIs, engines, formats, public entry points, or mixed broad/trivial work | Split: each atomic slice to its own `developer`; only the clustered remainder to `team_planner`. |
| Benchmark, fail-to-pass, migration, compatibility, or unresolved owner | Peel atomic pieces to `developer` first; residual clustered/unresolved work to `team_planner`. |
| Root-level evidence sweep after producers finish | `validator` with producer deps |

## Level Shape

| Situation | Action |
| --- | --- |
| Crowded level | Group by owner family or mechanism. |
| One broad `developer` | Make it expandable (`team_planner`) unless exact-owner evidence makes it atomic. |
| Many scout candidates | Scout superficially by boundary, then split into expandable tasks. |
| Many tiny variants under one mechanism | One task or one expandable task, not many thin siblings. |
| Unrelated owner families | Several siblings, grouped by boundary. |

## DAG Patterns

```text
Caption: parallel producers with one validator join.

api-serializer (developer) ----\
cli-renderer (team_planner) ----> same-payload-validator (validator)
```

```text
Caption: sequential output dependency.

compat-guard (developer) -> adapter-callsite (developer) -> adapter-validator (validator)
```

```text
Caption: exact work runs beside expandable work.

config-loader-fix (developer) ----------------\
storage-engine-planning (team_planner) --------> root-output-validator (validator)
```

| Dependency check | Rule |
| --- | --- |
| Output dependency | Add an edge only when one task consumes another task's output. |
| Validator coverage | A validator depends on every producer it verifies. |
| Related files | Shared directories alone do not create `deps`. |
| Uncertainty | Put uncertain ownership in `spec.detail`, not in fake ordering. |

## Payload Shape

```ts
submit_plan({ new_tasks: NewTaskDefinition[] })
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

| Field | Contract |
| --- | --- |
| `id` | Unique lower-kebab id in this payload. |
| `agent` | `developer`, `team_planner`, or `validator`. |
| `spec.goal` | Concrete outcome expected from the task. |
| `spec.detail` | Owner evidence, scope, uncertainty, and dependency context. |
| `spec.acceptance_criteria` | Commands, pytest ids, expected evidence, and no skip/xfail closure. |
| `deps` | Same-payload ids only. |
| `scope_paths` | Repo-relative production paths or directories. |

## Final Checklist

| # | Check |
| --- | --- |
| 1 | Every task has `id`, `agent`, `spec`, `deps`, and `scope_paths`. |
| 2 | `deps` resolve within this payload and express output order or validator coverage. |
| 3 | Expandable or unresolved slices use `team_planner`. |
| 4 | Named failing clusters have a producer owner or expandable owner. |
| 5 | Tests and benchmark ids stay in `spec`; production/scout paths stay exact in `scope_paths`. |
| 6 | The final assistant action is `submit_plan(...)` with no trailing prose. |
