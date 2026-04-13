# Plan JSON Contract
Use this reference immediately before emitting final plan JSON.

## Shape rules

- Emit `{"tasks": [...], "rationale": "..."}` as the tool-call payload shape.
- Finish the benchmark-surface ledger, deps, and task prose before loading this reference.
- After this reference loads, the very next terminal action must be `submit_plan(tasks=[...], rationale="...")`.
- No more tool calls, ownership recounts, or task-count debates are allowed after this reference loads.
- Never load this reference in parallel with `root-plan-self-check`.
- Must use `agent` only for registered workers: `developer`, `validator`, or `team_planner`.
- Must use `id` for the lane label.
- Must keep `deps` as a top-level item field.
- Must emit each `id` only once.
- Keep each task on the runtime `TaskSpec` shape: `id`, `task`, `agent`, `deps`, `scope_paths`, `cascade_policy`.
- The `task` field is the agent's sole briefing. Put exact owner, retry target, and recovery question there.
- Use exact live-confirmed or explorer-confirmed paths in `scope_paths`; if the exact owner is still uncertain, keep the broader boundary and assign it to `team_planner`.
- Keep at most one terminal validator in a submitted plan.
- Before loading this reference, confirm that the terminal validator depends on every terminal non-validator sibling. Do not learn that from a submit error.
- Do not submit an expandable `developer`.
- Do not serialize the whole layer into eight atomic developers only because all owners are known.
- Reload the ending chain sequentially if the self-check never finished.

## Failure-surface rules

- Freeze a tiny benchmark-surface ledger from the exact prompt paths or ids plus any validator-backed downgrades.
- On any submit retry, edit benchmark paths only by copying from that frozen ledger or exact validator packet text.
- Keep only those exact nodes or broaden to that same prompt file path; never substitute a same-family sibling node.
- If validation rejects a guessed benchmark node, keep only the validator-backed file path or remove that narrow node entirely.
- If no exact prompt, parent, scout, or validator-backed benchmark surface exists for one narrow lane after repair, omit that uncertain node instead of guessing another sibling.
- If a scout disproved an exact file, that file cannot appear in `tasks`, `scope_paths`, `task`, or `rationale`.
- A structure-only listing or import intuition is not "live-confirmed" owner evidence. If a scout disproved an exact file or marked a directory tests-only, do not replace that branch with a sibling exact file; broaden to the last confirmed parent boundary and keep it on `team_planner`.

## Few-shot examples

- Example: root explorers already mapped `hdf.py`, `parquet/`, `groupby.py`, and five tiny exact files.
  Emit `developer(hdf_fix)` plus expandable `team_planner` items like `parquet_child` or `groupby_child`, then direct tiny-file developers or one residual child planner for the rest.
- Example: the index was cold and the first wave only confirmed `dask/dataframe/` broadly.
  Keep `scope_paths=["dask/dataframe/"]` on a `team_planner` item. Do not emit `dask/dataframe/utils_dataframe.py` or another guessed leaf path.
- Example:
  ```json
  {
    "good_after_load": [
      "load_skill_reference(plan-json-contract)",
      "submit_plan(...)"
    ],
    "bad_after_load": [
      "load_skill_reference(plan-json-contract)",
      "write another ownership recap",
      "debate task counts"
    ]
  }
  ```
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
