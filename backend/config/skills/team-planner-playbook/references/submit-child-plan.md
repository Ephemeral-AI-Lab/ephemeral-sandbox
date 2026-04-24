# Team Planner Submit Child Plan Reference

This reference supports Stage 3 synthesis. Load it with:

```text
load_skill_reference(
  skill_name="team-planner-playbook",
  reference_name="submit-child-plan"
)
```

It is most useful after task context is loaded, the owner ledger is written, and any useful scout wave has joined or been intentionally skipped. Loading it does not freeze the workflow: keep newly discovered uncertainty explicit, make only bounded routing checks, and delegate unresolved slices to another child planner when depth allows, or to a max-depth diagnostic/repair lane.

## Routing Flow

```text
Caption: child planner routing with depth.

owner slice
  |-- exact owner + one mechanism -----------------> developer
  |-- expandable + grandchild_depth <= max_depth --> team_planner
  |-- expandable + max depth reached -------------> broader developer/validator split
  `-- same-payload verification ------------------> validator
```

| Slice signal | Route |
| --- | --- |
| One owner, one mechanism, focused verification | `developer` |
| Broad, clustered, matrix-shaped, mixed, or unresolved and depth remains | child `team_planner` |
| Broad or unresolved at max depth | direct per-mechanism `developer` tasks plus verification/diagnostic wording |
| Same-layer evidence sweep | optional `validator` depending on the producers it checks |

## Atomic vs Expandable

```text
Caption: preserve hierarchy instead of flattening clusters.

single clear leaf -> developer
cluster           -> child planner when depth allows
max-depth cluster -> split by mechanism with uncertainty in detail
```

| Atomic enough for current-layer `developer` | Expandable path |
| --- | --- |
| Live or inherited evidence names one owner file, symbol, or tight production surface | Owner is guessed, inherited as a cluster, or still unresolved |
| One failure mechanism explains the slice | Multiple mechanisms, APIs, backends, formats, or public entry points |
| Verification is focused and coherent | Verification is a broad benchmark, migration, compatibility, or matrix sweep |
| Scope is small enough for one developer pass | Four or more independent leaf fixes |

When uncertain and depth remains, use `team_planner`. At max depth, keep the uncertainty visible in `spec.detail` and avoid one catch-all developer task.

## DAG Level Size

Each level should be easy to scan and schedule.

| Situation at this level | Action |
| --- | --- |
| Crowded with many siblings | Group by owner family or mechanism; delegate the cluster to a child `team_planner` when depth remains. |
| One broad `developer` task alone | Check whether it should be a `team_planner` instead. |
| Many tiny variants under one mechanism | One atomic task or one child planner — not many thin-wrapper siblings. |
| Unrelated owner families | Several siblings or child planners, grouped by boundary. |

## Coverage And Evidence

| Item | Rule |
| --- | --- |
| Inherited failing targets | Preserve concrete pytest ids, variants, and file-level commands verbatim in `spec.detail` or `spec.acceptance_criteria`. |
| Tests and benchmark ids | Treat as evidence, not `scope_paths`, unless the user asked for test repair. |
| Scout gaps | Missing notes, cold paths, and adjacent-owner hypotheses stay as uncertainty unless live scout evidence proves the path. |
| Fail-to-pass closure | Do not close named targets by skip, xfail, clear `ImportError`, missing optional dependency, or "not supported" prose. |
| Validators | Optional at this layer. If included as terminal verification, depend on every same-payload producer it verifies, including child planners. |

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
| `spec.detail` | Owner evidence, inherited context, exact scope, constraints, and uncertainty. |
| `spec.acceptance_criteria` | Concrete verification commands or pytest ids and expected evidence. |
| `deps` | Same-payload ids only; parent/dependency UUIDs are context, not `deps` entries. |
| `scope_paths` | Repo-relative production paths proven by inherited context or scout evidence. |

Submit top-level `new_tasks` only. Do not include `summary`, `output`, `parent_id`, sandbox-absolute paths, or prose after the tool call.

## Payload Pattern

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-replan-rewire",
      agent: "developer",
      spec: {
        goal: "Repair the focused replan rewire invariant.",
        detail: "Own backend/src/team/task_center.py. Parent context and notes point to one mutation path; no child split is needed.",
        acceptance_criteria: "Run uv run pytest backend/tests/team/test_replan_workflow.py -q and report exit code plus changed behavior."
      },
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"]
    },
    {
      id: "plan-submission-policy",
      agent: "team_planner",
      spec: {
        goal: "Decompose submission policy work across schema, runtime policy, and prompt rendering.",
        detail: "Inherited evidence spans multiple owner families below backend/src/tools/submission and backend/src/prompt.",
        acceptance_criteria: "Child plan preserves inherited pytest ids and assigns each cluster to an owner or diagnostic lane."
      },
      deps: [],
      scope_paths: ["backend/src/tools/submission", "backend/src/prompt"]
    },
    {
      id: "val-child",
      agent: "validator",
      spec: {
        goal: "Verify all same-payload producer lanes.",
        detail: "Verify dev-replan-rewire and plan-submission-policy after both finish. Report gaps to the owning lane.",
        acceptance_criteria: "Run the focused replan suite and any inherited submission/prompt checks; report failing ids and owner gaps."
      },
      deps: ["dev-replan-rewire", "plan-submission-policy"],
      scope_paths: ["backend/src/team", "backend/src/tools/submission", "backend/src/prompt"]
    }
  ]
})
```

## DAG Patterns

```text
Caption: parallel child producers with optional validator join.

dev-replan-rewire ----\
plan-submission-policy -> val-child
```

```text
Caption: child planner output can gate a downstream integration lane.

plan-provider-contract -> dev-provider-bridge
plan-provider-contract -> val-provider
dev-provider-bridge   -> val-provider
```

## Final Checklist

| # | Check |
| --- | --- |
| 1 | Each task has `id`, `agent`, `spec`, `deps`, and `scope_paths`. |
| 2 | `deps` resolve within this payload. |
| 3 | Expandable slices use `team_planner` while depth remains. |
| 4 | At max depth, split broad work by mechanism instead of one catch-all lane. |
| 5 | Inherited test ids and benchmark targets stay verbatim in `spec`. |
| 6 | The final assistant action is `submit_plan(...)`. |
