# Plan JSON Contract
Use this reference immediately before emitting final plan JSON.

## Shape rules
- Must emit `{"tasks": [...], "rationale": "..."}` as the tool-call payload shape.
- Finish the benchmark-surface ledger, deps, and task prose before loading this reference.
- After this reference loads, the very next terminal action must be `submit_plan(tasks=[...], rationale="...")`.
- Never load this reference in parallel with `root-plan-self-check`; wait for that tool call to finish first.
- Must use `agent` only for registered workers: `developer`, `validator`, or `team_planner`.
- Must use `id` for the lane label such as `compat_fix` or `validate_misc`.
- Must keep `deps` as a top-level item field.
- Must keep each task on the runtime `TaskSpec` shape: `id`, `task`, `agent`, `deps`, `scope_paths`, `cascade_policy`.
- The `task` field is the agent's sole briefing. Put exact owner, retry target, and recovery question there.
- Must emit each `id` only once.
- Must keep at most one terminal validator in a submitted plan.
- If a terminal validator exists, its `deps` must list every terminal non-validator sibling in that plan.
- Must not submit placeholder scout scaffolds such as `plan-anchor-*`, `*_scout`, or `developer_override`.
- Planner-role items are expandable. Do not submit an expandable `developer`.
- Branch-local validators are valid only when they are non-terminal because later work or the terminal validator depends on them.
- If two exact-file slices arrived through separate scout artifacts, keep them as separate leaves or place them behind one residual child planner.

## Failure-surface rules
- Freeze a tiny benchmark-surface ledger from the exact prompt paths or ids plus any validator-backed downgrades.
- On any submit retry, edit benchmark paths only by copying from that frozen ledger or exact validator packet text.
- When one lane owns many failures from the same benchmark file, prefer that exact benchmark file path over guessed same-family nodes.
- Keep only those exact nodes or broaden to that same prompt file path; never substitute a same-family sibling node.
- Preserve the exact benchmark file basename and directory segments already quoted by the prompt, scout notes, or validator evidence.
- If an inherited parent lane only names an exact benchmark file path, keep that file path until prompt, scout, or validator evidence supplies an exact existing node id.
- Never normalize one benchmark path into a nearby sibling such as `test_utils_dataframe.py` -> `test_utils.py`.
- If validation rejects a guessed benchmark node, first fall back to the exact benchmark file path already named in the prompt or notes.
- If no exact prompt, parent, scout, or validator-backed benchmark surface exists for one narrow lane after that repair, omit that uncertain benchmark node instead of guessing another sibling.

## Few-shot examples
- Example: root scouts already mapped `hdf.py`, `parquet/`, `groupby.py`, and five tiny exact files.
  Emit `developer(hdf_fix)` plus expandable `team_planner` items like `parquet_child` or `groupby_child`, then direct tiny-file developers or one residual child planner for the rest. Do not serialize the whole layer into eight atomic developers only because all owners are known.
- Example: the parquet package still needs internal decomposition.
  Use `{"id":"parquet_child","agent":"team_planner",...}` or collapse it to one bounded atomic developer if the scope is already leaf-ready.
  Do not target a developer for work that still needs deeper decomposition.
- Example:
  ```json
  {
    "tasks": [
      {"id": "dev-hdf", "agent": "developer", "deps": [], "scope_paths": ["pkg/io/hdf.py"], "cascade_policy": "cancel", "task": "Restore the shared HDF export in pkg/io/hdf.py. Reproduce and keep verification on pytest pkg/tests/test_hdf.py -x."},
      {"id": "plan-parquet", "agent": "team_planner", "deps": [], "scope_paths": ["pkg/io/parquet/"], "cascade_policy": "cancel", "task": "Decompose parquet IO failures across engine backends."},
      {"id": "val-root", "agent": "validator", "deps": ["dev-hdf", "plan-parquet"], "scope_paths": ["pkg/io/hdf.py", "pkg/io/parquet/"], "cascade_policy": "cancel", "task": "Run the terminal verification gate for the root layer."}
    ],
    "rationale": "One direct leaf is ready, parquet stays expandable, and the single terminal validator depends on every terminal non-validator sibling."
  }
  ```
  Do not emit `val-hdf` and `val-parquet` as separate terminal siblings at the same layer.
- Example: you queued `root-plan-self-check` and this contract together.
  Reload the ending chain sequentially if the self-check never finished; otherwise keep the literal benchmark ledger and emit the plan tool call next.
