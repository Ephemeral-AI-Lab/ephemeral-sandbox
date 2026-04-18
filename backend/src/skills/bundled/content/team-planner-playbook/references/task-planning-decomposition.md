# Task Planning Decomposition

Use this reference only after ownership is already clear enough to draft the DAG.
On a fresh root, use it only after a first-wave scout has been launched and its notes reviewed, or after existing Task Center scout notes already cover the owner slices.
If any nameable first-wave explorer is still unlaunched, stop and return to launcher reconciliation instead of shaping the DAG.

## Task/Goal

- Ownership is already clear enough to shape lanes, but you still need to decide atomic vs expandable work.

## Avoid

- Split distinct owner clusters into separate execution lanes.
- Keep ready work concrete and residual work explicit before `plan-json-contract`.
- Use deps only for real sequencing, shared-risk branch cuts, or verification boundaries.
- Let child planners own their own deeper validation instead of using parent validators as decoration.
- Add validators only when they reduce uncertainty for concrete lanes.
- Keep the plan between 2 items and `max_plan_size`.
- Refresh with `read_task_note(...)` and respect freshness signals before turning a formerly broad boundary into an exact-file leaf.
- Never hide unresolved owner clusters behind validator-only coverage.
- Never call a leftovers lane atomic unless one shared live owner explains every file and benchmark verify surface in it.
- Do not create one atomic "misc fixes" lane just because those residual slices are individually small.
- Do not collapse those unrelated files into one atomic developer just to save root-plan slots.

## Workflow

1. Must keep decomposition explicit enough that the next layer can act without reopening the same ownership question.
2. Use `developer` for atomic tasks: one owner slice, one patch surface, and one verification family are already clear enough for a leaf worker.
3. Use `team_planner` for expandable tasks: the work still hides multiple owner slices, region-level decomposition inside one broad file, or more ready work than the current layer should flatten.
4. Preserve at least one direct ready leaf lane whenever live evidence already supports it, even if sibling branches still need child planners.
5. Treat exact-file pairs as separate owner slices unless explorer notes already proved one shared helper or boundary that truly owns both.
6. If cold CI left a slice on a broad directory/package boundary, keep that lane expandable until live notes confirm an exact file.
7. Use `validator` for validation tasks, typically as the terminal gate that depends on every terminal non-validator sibling in the submitted layer.

## Shared-file detection

Before shaping the DAG, check whether any file appears in scout notes or `ci_query_symbol(..., references=true)` results for more than one owner slice being split into parallel lanes. A file imported or modified by two planned developer scopes is a **shared file**.

When a shared file is found:
- If one owner slice is the primary author and others only read it, assign the shared file to the primary author's scope and add a dep edge from consumers.
- If both slices need to edit the shared file, create a dedicated sequenced `developer` task for that file and make both consumer lanes depend on it.
- Never split a shared file across parallel developers with no dep edge between them.

Use `ci_query_symbol(symbol, references=true)` on symbols that appear as imports in multiple scout notes to confirm cross-slice usage before finalising the DAG.

## Expected Outcome

- The submitted layer shows clear direct work, explicit residual work, and no hidden shared-file conflicts.

### Example task graph: one direct leaf plus one expandable branch

```json
{
  "new_tasks": [
    {
      "id": "dev-hdf", "description": "Fix HDF owner surface",
      "name": "developer",
      "deps": [],
      "scope_paths": ["pkg/io/hdf.py"],
      "spec": "1. Goal: Fix the exact HDF owner surface and keep verification on the named failing target.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "plan-parquet", "description": "Decompose parquet owner surface",
      "name": "team_planner",
      "deps": [],
      "scope_paths": ["pkg/io/parquet/"],
      "spec": "1. Goal: Decompose the unresolved parquet owner surface into direct work.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "val-root", "description": "Validate HDF and parquet",
      "name": "validator",
      "deps": ["dev-hdf", "plan-parquet"],
      "scope_paths": ["pkg/io/hdf.py", "pkg/io/parquet/"],
      "spec": "1. Goal: Run the terminal validation gate for this layer.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    }
  ]
}
```

### Example task graph: shared-file sequencing

```json
{
  "new_tasks": [
    {
      "id": "dev-shared-config", "description": "Repair shared config",
      "name": "developer",
      "deps": [],
      "scope_paths": ["pkg/config.py"],
      "spec": "1. Goal: Repair the shared config surface used by both downstream slices.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "dev-reader", "description": "Fix reader logic",
      "name": "developer",
      "deps": ["dev-shared-config"],
      "scope_paths": ["pkg/reader.py"],
      "spec": "1. Goal: Fix the reader-specific logic after the shared config repair lands.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "dev-writer", "description": "Fix writer logic",
      "name": "developer",
      "deps": ["dev-shared-config"],
      "scope_paths": ["pkg/writer.py"],
      "spec": "1. Goal: Fix the writer-specific logic after the shared config repair lands.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "val-root", "description": "Validate sequenced shared-file plan",
      "name": "validator",
      "deps": ["dev-reader", "dev-writer"],
      "scope_paths": ["pkg/config.py", "pkg/reader.py", "pkg/writer.py"],
      "spec": "1. Goal: Run the terminal validation gate for the sequenced shared-file plan.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    }
  ]
}
```

### Example task graph: cold-CI broad boundary stays expandable

```json
{
  "new_tasks": [
    {
      "id": "plan-dataframe-io", "description": "Decompose dataframe IO",
      "name": "team_planner",
      "deps": [],
      "scope_paths": ["pkg/dataframe/io/"],
      "spec": "1. Goal: Decompose the still-broad dataframe IO owner surface after a cold-CI opening.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "dev-config", "description": "Fix config owner surface",
      "name": "developer",
      "deps": [],
      "scope_paths": ["pkg/config.py"],
      "spec": "1. Goal: Fix the exact config owner surface already confirmed by live evidence.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "val-root", "description": "Validate broad-boundary plan",
      "name": "validator",
      "deps": ["plan-dataframe-io", "dev-config"],
      "scope_paths": ["pkg/dataframe/io/", "pkg/config.py"],
      "spec": "1. Goal: Run the terminal validation gate for the broad-boundary plan.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    }
  ]
}
```

### Example task graph: broad single file stays expandable for region split

```json
{
  "new_tasks": [
    {
      "id": "plan-groupby", "description": "Split groupby regions",
      "name": "team_planner",
      "deps": [],
      "scope_paths": ["pkg/groupby.py"],
      "spec": "1. Goal: Split the broad groupby file into region-level work items with distinct verification families.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "dev-hdf", "description": "Fix HDF owner surface",
      "name": "developer",
      "deps": [],
      "scope_paths": ["pkg/io/hdf.py"],
      "spec": "1. Goal: Fix the exact HDF owner surface already ready for direct execution.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    },
    {
      "id": "val-root", "description": "Validate mixed expandable plan",
      "name": "validator",
      "deps": ["plan-groupby", "dev-hdf"],
      "scope_paths": ["pkg/groupby.py", "pkg/io/hdf.py"],
      "spec": "1. Goal: Run the terminal validation gate for the mixed direct-plus-expandable plan.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Stay within the listed scope_paths unless live evidence requires a narrower or broader confirmed owner.\n4. Context: This task is part of the submitted team plan.\n5. Acceptance Criteria: Submit the required terminal outcome with concrete evidence."
    }
  ]
}
```
