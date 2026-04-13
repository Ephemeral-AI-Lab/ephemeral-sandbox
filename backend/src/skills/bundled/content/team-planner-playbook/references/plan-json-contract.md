# Plan JSON Contract
Use this reference immediately before emitting final plan JSON.

## Shape rules

- Emit `{"tasks": [...], "rationale": "..."}` as the tool-call payload shape.
- Finish the benchmark-surface ledger, deps, and task prose before loading this reference.
- After this reference loads, the very next terminal action must be `submit_plan(tasks=[...], rationale="...")`.
- Never load this reference in parallel with `root-plan-self-check`.
- Use `agent` only for registered workers: `developer`, `validator`, or `team_planner`.
- Keep each task on the runtime `TaskSpec` shape: `id`, `task`, `agent`, `deps`, `scope_paths`, `cascade_policy`.
- The `task` field is the agent's sole briefing. Put exact owner, retry target, and recovery question there.
- Use exact live-confirmed or explorer-confirmed paths in `scope_paths`; if the exact owner is still uncertain, keep the broader boundary and assign it to `team_planner`.
- Keep at most one terminal validator in a submitted plan.

## Failure-surface rules

- Freeze a tiny benchmark-surface ledger from the exact prompt paths or ids plus any validator-backed downgrades.
- On any submit retry, edit benchmark paths only by copying from that frozen ledger or exact validator packet text.
- Keep only those exact nodes or broaden to that same prompt file path; never substitute a same-family sibling node.
- If no exact prompt, parent, scout, or validator-backed benchmark surface exists for one narrow lane after repair, omit that uncertain node instead of guessing another sibling.

## Few-shot examples

- Example: root explorers already mapped `hdf.py`, `parquet/`, `groupby.py`, and five tiny exact files.
  Emit `developer(hdf_fix)` plus expandable `team_planner` items like `parquet_child` or `groupby_child`, then direct tiny-file developers or one residual child planner for the rest.
- Example: the index was cold and the first wave only confirmed `dask/dataframe/` broadly.
  Keep `scope_paths=["dask/dataframe/"]` on a `team_planner` item. Do not emit `dask/dataframe/utils_dataframe.py` or another guessed leaf path.
- Example:
  ```json
  {
    "tasks": [
      {"id": "dev-hdf", "agent": "developer", "deps": [], "scope_paths": ["pkg/io/hdf.py"], "cascade_policy": "cancel", "task": "Restore the shared HDF export in pkg/io/hdf.py. Reproduce and keep verification on pytest pkg/tests/test_hdf.py -x."},
      {"id": "plan-parquet", "agent": "team_planner", "deps": [], "scope_paths": ["pkg/io/parquet/"], "cascade_policy": "cancel", "task": "Decompose parquet IO failures across engine backends."},
      {"id": "val-root", "agent": "validator", "deps": ["dev-hdf", "plan-parquet"], "scope_paths": ["pkg/io/hdf.py", "pkg/io/parquet/"], "cascade_policy": "cancel", "task": "Run the terminal verification gate for the root layer."}
    ],
    "rationale": "One direct leaf is ready, parquet stays expandable, and the terminal validator depends on every terminal non-validator sibling."
  }
  ```
