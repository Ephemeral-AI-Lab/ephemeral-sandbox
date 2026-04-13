# Task Planning Decomposition

Use this reference only after ownership is already clear enough to draft the DAG.
If any nameable first-wave explorer is still unlaunched, stop and return to launcher reconciliation instead of shaping the DAG.

## Decide atomic vs expandable

0. Must keep decomposition explicit enough that the next layer can act without reopening the same ownership question.
1. Make a lane atomic when one owner slice, one patch surface, and one verification family are already clear enough for a leaf worker.
2. Make a lane expandable when it still hides multiple owner slices, region-level decomposition inside one broad file, or more ready work than the current layer should flatten.
3. Make a lane expandable when the owner is already a package, directory, or broad single file and the next useful decision is internal lane shaping rather than direct patching.
4. Make a lane expandable when the alternative atomic lane would own several unrelated exact files merely because each slice is small.
5. Preserve at least one direct ready leaf lane whenever live evidence already supports it, even if sibling branches still need child planners.
6. Treat exact-file pairs as separate owner slices unless explorer notes already proved one shared helper or boundary that truly owns both.
7. If cold CI left a slice on a broad directory/package boundary, keep that lane expandable until live notes confirm an exact file.

## Shared-file detection

Before shaping the DAG, check whether any file appears in scout notes or `ci_query_references` results for more than one owner slice being split into parallel lanes. A file imported or modified by two planned developer scopes is a **shared file**.

When a shared file is found:
- If one owner slice is the primary author and others only read it, assign the shared file to the primary author's scope and add a dep edge from consumers.
- If both slices need to edit the shared file, create a dedicated sequenced `developer` task for that file and make both consumer lanes depend on it.
- Never split a shared file across parallel developers with no dep edge between them.

Use `ci_query_references(file_path, symbol)` on files that appear as imports in multiple scout notes to confirm cross-slice usage before finalising the DAG.

## DAG shaping rules

- Split distinct owner clusters into separate execution lanes.
- Keep ready work concrete and residual work explicit before `plan-json-contract`.
- Use deps only for real sequencing, shared-risk branch cuts, or verification boundaries.
- Let child planners own their own deeper validation instead of using parent validators as decoration.
- Add validators only when they reduce uncertainty for concrete lanes.
- Keep the plan between 2 items and `max_plan_size`.
- Refresh with `read_notes(...)` and respect freshness signals before turning a formerly broad boundary into an exact-file leaf.
- Never hide unresolved owner clusters behind validator-only coverage.
- Never call a leftovers lane atomic unless one shared live owner explains every file and benchmark verify surface in it.
- Do not create one atomic "misc fixes" lane just because those residual slices are individually small.
- Do not collapse those unrelated files into one atomic developer just to save root-plan slots.

## Few-shot examples

- Example: root evidence clearly isolates `pkg/io/hdf.py`, while `pkg/io/parquet/` and `pkg/groupby.py` still each need their own decomposition.
  Emit `developer(hdf)` now, then two child planners for parquet and groupby.
- Example: the index was cold, so the first wave only confirmed `dask/dataframe/io/` broadly plus exact leaves for `dask/config.py` and `dask/compatibility.py`.
  Keep `dask/dataframe/io/` as `team_planner`; do not force `dask/dataframe/io/parquet.py` or `dask/dataframe/utils_dataframe.py` into atomic leaves.
- Example: one huge `pkg/groupby.py` file contains separate `cov`, `unique`, and `value_counts` regions with different verification families.
  Use a child planner for the file-level region split even though the owner file is singular.
