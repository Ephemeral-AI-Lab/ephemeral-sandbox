# Plan JSON Contract
Use this reference immediately before calling `submit_task_plan(...)`.

## Shape rules

- For planner submissions, call `submit_task_plan(new_tasks=[...])`.
- Each `new_tasks` item must follow the runtime shape: `id`, `name`, `objective`, `deps`, `scope_paths`.
- Finish the benchmark-surface ledger, deps, and task prose before loading this reference.
- After this reference loads, the very next action must be calling `submit_task_plan(new_tasks=[...])`. Do not make any more non-submission tool calls in the main loop after this reference loads.
- No more ownership recounts or task-count debates are allowed after this reference loads.
- Never load this reference in parallel with `root-plan-self-check`.
- Must use `name` with an exact registered agent name such as `developer`, `validator`, or `team_planner`.
- Must use `id` for the lane label — a short unique string used to wire `deps`.
- Must keep `deps` as a top-level item field.
- Must emit each `id` only once.
- The `objective` field is the agent's sole briefing. Put exact owner, retry target, and recovery question there.
- Use exact live-confirmed or explorer-confirmed paths in `scope_paths`; if the exact owner is still uncertain, keep the broader boundary and assign it to `team_planner`.
- Keep at most one terminal validator in a submitted plan.
- Before loading this reference, confirm that the terminal validator depends on every terminal non-validator sibling. Do not learn that from a submit error.
- Validator tasks will be normalized to `cascade_policy="continue"` automatically; developer and `team_planner` tasks use the default strict dependency policy.
- Count crowded-layer width using non-planner, non-validator tasks only: `6 developers + 1 validator` is fine, but `7 developers + 1 validator` still needs a residual `team_planner`.
- On crowded layers, zero-expandable all-developer plans are invalid when any residual branch is still multi-file, shared-risk, or otherwise broad enough for child decomposition.
- Reload the ending chain sequentially if the self-check never finished.

## Failure-surface rules

- Freeze a tiny benchmark-surface ledger from the exact prompt paths or ids plus any validator-backed downgrades.
- On any submit retry, edit benchmark paths only by copying from that frozen ledger or exact validator packet text.
- Keep only those exact nodes or broaden to that same prompt file path; never substitute a same-family sibling node.
- If validation rejects a guessed benchmark node, keep only the validator-backed file path or remove that narrow node entirely.
- If no exact prompt, parent, scout, or validator-backed benchmark surface exists for one narrow lane after repair, omit that uncertain node instead of guessing another sibling.
- If a scout disproved an exact file, that file cannot appear in `tasks`, `scope_paths`, `objective`, or `rationale`.
- If a scout disproved a benchmark-import path, do not emit a task whose main job is to create a compat/re-export file at that missing path unless live production references also name it.
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
      "call submit_task_plan(new_tasks=[...])"
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
    "new_tasks": [
      {"id": "dev-hdf", "name": "developer", "deps": [], "scope_paths": ["pkg/io/hdf.py"], "objective": "Restore the shared HDF export in pkg/io/hdf.py. Reproduce and keep verification on pytest pkg/tests/test_hdf.py -x."},
      {"id": "plan-parquet", "name": "team_planner", "deps": [], "scope_paths": ["pkg/io/parquet/"], "objective": "Decompose parquet IO failures across engine backends."},
      {"id": "val-root", "name": "validator", "deps": ["dev-hdf", "plan-parquet"], "scope_paths": ["pkg/io/hdf.py", "pkg/io/parquet/"], "objective": "Run the terminal verification gate for the root layer."}
    ]
  }
  ```
