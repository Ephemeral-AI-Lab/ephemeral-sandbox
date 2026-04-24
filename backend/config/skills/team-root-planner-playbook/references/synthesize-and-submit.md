# Root Planner Synthesize and Submit Reference

This reference supports Stage 3 synthesis. Load it with:


It is most useful after the owner ledger is complete and any useful scout wave has joined or been intentionally skipped. Loading it does not freeze the workflow: keep newly discovered uncertainty explicit, make only bounded routing checks, and route unresolved slices to a child `team_planner` or diagnostic task.

## Routing Flow

```text
Caption: root planner routing from owner-ledger rows.

owner slice
  |-- exact owner + one mechanism + bounded verification -> developer
  |-- broad, clustered, matrix-shaped, mixed, unresolved -> team_planner
  `-- same-payload verification after producers ---------> validator
```

| Slice signal | Route |
| --- | --- |
| One production owner, one clear mechanism, one coherent verification path | `developer` |
| Benchmark, fail-to-pass, migration, compatibility, multi-API, multi-family, or unresolved owner | `team_planner` |
| Root-level evidence sweep after producers finish | `validator` depending on the producers it checks |

Prefer top-down decomposition. The root should split boundaries and hand expandable regions to child planners; it does not need to discover every leaf fix.

## Atomic vs Expandable

```text
Caption: classification is a routing aid, not a proof burden.

clear leaf      -> developer
unclear cluster -> team_planner
too many leaves -> team_planner
```

| Atomic enough for `developer` | Expandable, route to `team_planner` |
| --- | --- |
| Live evidence names one owner file, symbol, or tight production surface | Owner is guessed, shortlisted, or only test-derived |
| One failure mechanism explains the slice | Several mechanisms, APIs, backends, formats, or public entry points |
| Verification is focused and coherent | Verification is a benchmark matrix, migration sweep, or broad suite |
| Scope is small enough for one developer pass | Four or more independent leaf fixes, even if each is small |

When unsure, route to `team_planner` and preserve the uncertainty in `spec.detail`.

## DAG Level Size

Each level should be easy to scan and schedule.

| Situation at this level | Action |
| --- | --- |
| Crowded with many siblings | Group by owner family or mechanism; delegate the cluster to a child `team_planner`. |
| One broad `developer` task alone | Check whether it should be a `team_planner` instead. |
| Many tiny variants under one mechanism | One atomic task or one child planner — not many thin-wrapper siblings. |
| Unrelated owner families | Several siblings or child planners, grouped by boundary. |

## Coverage And Evidence

| Item | Rule |
| --- | --- |
| Named failing clusters | Give each cluster a repair/decomposition owner, or explicitly hand it to a child planner. |
| Tests and benchmark ids | Keep them in `spec.detail` or `spec.acceptance_criteria`, not `scope_paths`, unless the user asked for test repair. |
| Scout gaps | Keep missing notes, cold paths, and disproved exact files as uncertainty; do not turn guesses into `scope_paths`. |
| Skip/xfail/import closure | Do not treat skipped, expected-failed, missing optional dependency, or clear `ImportError` outcomes as passing fail-to-pass closure. |
| Validator | Use one terminal validator when the root payload needs a same-layer join. It depends on every producer it verifies, including child planners. |

## Submission Shape

```ts
submit_plan({ new_tasks: NewTaskDefinition[] })
```

```ts
type TaskSpec = {
  goal: string;
  detail: string;
  acceptance_criteria: string;
};

type NewTaskDefinition = {
  id: string;
  agent: "developer" | "validator" | "team_planner";
  spec: TaskSpec;
  deps: string[];
  scope_paths: string[];
};
```

| Field | Contract |
| --- | --- |
| `id` | Unique lower-kebab id in this payload. |
| `agent` | `developer`, `team_planner`, or `validator`. |
| `spec.goal` | Concrete outcome expected from the task. |
| `spec.detail` | Owner evidence, scope, constraints, uncertainty, and dependency context. |
| `spec.acceptance_criteria` | Concrete commands, pytest ids, expected evidence, and no skip/xfail closure. |
| `deps` | Same-payload ids only; use edges for real output ordering or validator coverage. |
| `scope_paths` | Repo-relative production paths or directories. |

Submit top-level `new_tasks` only. Do not include `summary`, `output`, `parent_id`, or prose after the tool call.

## Payload Pattern

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-focused-owner",
      agent: "developer",
      spec: {
        goal: "Repair the focused production invariant.",
        detail: "Own backend/src/team/task_center.py. Scout evidence names one mutation path and no broader owner gap.",
        acceptance_criteria: "Run uv run pytest backend/tests/team/test_task_center.py -q and report exit code plus changed behavior."
      },
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"]
    },
    {
      id: "plan-runtime-cluster",
      agent: "team_planner",
      spec: {
        goal: "Decompose runtime failures across the owner families below backend/src/team.",
        detail: "The slice is clustered and includes unresolved ownership. Child planning should split developer leaves by owner/mechanism.",
        acceptance_criteria: "Child plan covers the named failing ids without skip, xfail, missing dependency, or test rewrite closure."
      },
      deps: [],
      scope_paths: ["backend/src/team"]
    },
    {
      id: "val-root",
      agent: "validator",
      spec: {
        goal: "Verify the root producer lanes after they finish.",
        detail: "Verify the focused developer lane and child planner output. Report uncovered clusters to the owning lane.",
        acceptance_criteria: "Run the focused task-center suite and the broader team suite; report failing ids and owners for any red evidence."
      },
      deps: ["dev-focused-owner", "plan-runtime-cluster"],
      scope_paths: ["backend/src/team"]
    }
  ]
})
```

## DAG Patterns

```text
Caption: parallel producers with one validator join.

dev-focused-owner  ----\
plan-runtime-cluster ---> val-root
```

```text
Caption: real output consumption creates a dependency; scope overlap alone does not.

dev-contract -> dev-consumer
dev-contract -> val-root
dev-consumer -> val-root
```

## Final Checklist

| # | Check |
| --- | --- |
| 1 | Each task has `id`, `agent`, `spec`, `deps`, and `scope_paths`. |
| 2 | `deps` resolve within this payload. |
| 3 | Expandable or unresolved slices use `team_planner`, not a catch-all `developer`. |
| 4 | Named failing clusters have a producer owner or child planner. |
| 5 | Tests stay as evidence in `spec`; production paths stay in `scope_paths`. |
| 6 | The final assistant action is `submit_plan(...)`. |
