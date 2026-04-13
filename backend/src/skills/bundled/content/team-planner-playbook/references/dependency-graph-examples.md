# Dependency Graph Examples

Use this reference immediately before final plan JSON when there are 4+ candidate siblings, one dominant owner plus residual single-file slices, or any temptation to create `misc_*` lanes.

## Lane smells

- An atomic lane that owns several unrelated exact files only because each slice is small is under-decomposed.
- Local ids such as `misc`, `remaining`, `assorted`, or `small_fixes` are a stop signal unless explorers already proved one shared owner.
- If a lane would verify several unrelated test files just to cover bundled leftovers, emit a residual child planner or split direct leaves instead.
- If a parent plan ends as direct developers for every mapped slice plus one terminal validator, depth probably collapsed too early.
- If a slice is still only confirmed at a directory/package boundary because the opener was cold, keep it expandable instead of inventing an exact-file leaf.

## Few-shot examples

- Example: root explorers land `pkg/io/hdf.py`, `pkg/io/parquet/`, `pkg/groupby.py`, `pkg/io/json.py`, `pkg/utils.py`, `pkg/cli.py`, `pkg/config.py`, and `pkg/compat.py`.
  Emit `developer(hdf)` now, child planners for `parquet` and `groupby`, then either several direct developers or one residual child planner for the remaining single-file slices.
- Example: the cold-CI opener only confirmed `dask/dataframe/` broadly plus exact leaves for `dask/config.py` and `dask/compatibility.py`.
  Keep `dask/dataframe/` as a `team_planner` child, emit the exact leaves directly, then finish with one terminal validator.
