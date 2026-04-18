# Plan JSON Contract
Use this reference as an optional final helper immediately before calling `submit_plan(...)`.

## Task/Goal

- You already have the owner ledger, deps, and task prose. Use the `submit_plan` tool schema directly if you do not need this final helper.

## Avoid

- Freeze a tiny benchmark-surface ledger from the exact prompt paths or ids plus any validator-backed downgrades.
- On any submit retry, edit benchmark paths only by copying from that frozen ledger or exact validator packet text.
- Keep only those exact nodes or broaden to that same prompt file path; never substitute a same-family sibling node.
- If validation rejects a guessed benchmark node, keep only the validator-backed file path or remove that narrow node entirely.
- If no exact prompt, parent, scout, or validator-backed benchmark surface exists for one narrow lane after repair, omit that uncertain node instead of guessing another sibling.
- If a scout disproved an exact file, that file cannot appear in `scope_paths` or `spec`.
- If a scout disproved a benchmark-import path, do not emit a task whose main job is to create a compat/re-export file at that missing path unless live production references also name it.
- A structure-only listing or import intuition is not "live-confirmed" owner evidence. If a scout disproved an exact file or marked a directory tests-only, do not replace that branch with a sibling exact file; broaden to the last confirmed parent boundary and keep it on `team_planner`.

## Workflow

- For planner submissions, call `submit_plan(new_tasks=[...])`.
- The only top-level keys are `new_tasks` and optional string `output`. Do not include `task_note`, `background`, `parent_id`, `rationale`, or `output: null`.
- Each `new_tasks` item must follow the runtime shape: `id`, `description`, `name`, `spec`, `deps`, `scope_paths`.
- Finish the benchmark-surface ledger, deps, and task prose before loading this reference.
- After this reference loads, the very next action must be calling `submit_plan(new_tasks=[...])`. Do not make any more non-submission tool calls in the main loop after this reference loads.
- Never load this reference in parallel with `root-plan-self-check`.
- Must use `name` with an exact registered agent name such as `developer`, `validator`, or `team_planner`.
- Must use `id` for the lane label — a short unique string used to wire `deps`.
- Must use `description` for a planner-authored short label under about 10 words; do not duplicate the full `spec`.
- Must keep `deps` as a top-level item field.
- Must emit each `id` only once.
- The `spec` field is the agent's sole briefing. Put exact owner, failure target, and recovery question there.
- Format every `spec` with numbered colon labels in this exact order: `1. Goal:`, `2. Environment:`, `3. Scope:`, `4. Context:`, `5. Acceptance Criteria:`. Do not use Markdown headings like `## Goal`.
- Use exact live-confirmed or explorer-confirmed paths in `scope_paths`; if the exact owner is still uncertain, keep the broader boundary and assign it to `team_planner`.
- Keep at most one terminal validator in a submitted plan.
- Before loading this reference, confirm that the terminal validator depends on every terminal non-validator sibling. Do not learn that from a submit error.
- Reviewer tasks are allowed to proceed after upstream failure; developer and `team_planner` tasks still use strict dependency handling.
- On crowded layers, keep at least one residual `team_planner` lane whenever unresolved work is still broad, shared-risk, or multi-file.

## Spec Examples

### Developer

```md
1. Goal: Fix the regression where `DataFrame.to_hdf()` drops categorical column metadata during round-trip serialization.
2. Environment: Work in the current repository checkout. Use the existing Python test environment and the team runtime tools available to `developer`. Prefer focused pytest runs before broader validation.
3. Scope: Start in `pkg/io/hdf.py` and `pkg/tests/io/test_hdf.py`. Do not edit parquet, CSV, or SQL IO code unless a live reference proves the HDF path delegates there.
4. Context: The failing target is `pkg/tests/io/test_hdf.py::test_categorical_roundtrip_preserves_categories`. Scout notes found the serialization path in `pkg/io/hdf.py::write_hdf` and the readback path in `pkg/io/hdf.py::read_hdf`. The root planner assigned this as an atomic owner slice because the failure is isolated to HDF metadata handling.
5. Acceptance Criteria: Add or update a regression test for categorical metadata preservation, implement the smallest HDF fix, run `pytest pkg/tests/io/test_hdf.py::test_categorical_roundtrip_preserves_categories -q`, and submit a success summary listing changed files and command results. If the focused test cannot run, submit a fail summary with the exact command, error, and nearest evidence gathered.
```

### Team Planner

```md
1. Goal: Decompose the parquet IO owner surface into executable child tasks that can run without overlapping writes.
2. Environment: Work in the current repository checkout with planning and inspection tools. Use Task Center notes first, then targeted symbol/file reads only where ownership is still unclear. Do not patch code or run implementation tests yourself.
3. Scope: Plan within `pkg/io/parquet/`, `pkg/tests/io/test_parquet.py`, and live-confirmed helpers imported by that package. Keep benchmark-only paths in prose, not `scope_paths`, unless they are also production owners.
4. Context: The parent planner confirmed three failing targets: `test_parquet_nullable_string_roundtrip`, `test_parquet_index_metadata_merge`, and `test_parquet_engine_fallback_error`. Scout notes show `pkg/io/parquet/core.py` owns schema normalization, while `pkg/io/parquet/engine.py` owns engine selection. The surface is too broad for one developer because schema fixes and engine fallback can be separated, but both may need a terminal validator.
5. Plan Rule: Submit separate developer lanes for schema normalization and engine fallback if ownership remains distinct. Sequence any shared-file work if both lanes touch the same file. Include one terminal validator depending on all terminal non-validator lanes.
6. Acceptance Criteria: Submit a valid child plan where every new task has concrete `scope_paths`, deps, and a structured `spec`; no parallel developer lanes write the same file without a dependency edge; and the validator covers all child implementation lanes.
```

### Validator

```md
1. Goal: Validate the HDF categorical round-trip fix and catch regressions in the adjacent HDF IO suite.
2. Environment: Work in the current repository checkout after upstream developer tasks complete or fail. Use available CI/test tools. Run diagnostics before the suite so import/name errors are reported clearly.
3. Scope: Inspect the final diff for `pkg/io/hdf.py` and `pkg/tests/io/test_hdf.py`. Validate with HDF-focused tests only; do not broaden to parquet or CSV unless the changed diff imports those paths.
4. Context: This validator depends on `dev-hdf-categorical`. The developer was expected to preserve categorical metadata for `DataFrame.to_hdf()`/readback and to add or update a regression test. The original failing target was `pkg/tests/io/test_hdf.py::test_categorical_roundtrip_preserves_categories`.
5. Acceptance Criteria: Run `ci_diagnostics(file_path="pkg/io/hdf.py")`, run `pytest pkg/tests/io/test_hdf.py::test_categorical_roundtrip_preserves_categories pkg/tests/io/test_hdf.py -q`, and submit a validation summary with commands, pass/fail status, and any remaining failure snippets. If upstream failed, still inspect available notes and submit a fail summary that preserves the developer failure context.
```

## Expected Outcome

```json
{
  "new_tasks": [
    {"id": "dev-hdf", "description": "Restore HDF export", "name": "developer", "deps": [], "scope_paths": ["pkg/io/hdf.py"], "spec": "1. Goal: Restore the shared HDF export in pkg/io/hdf.py and keep verification on the named failing target.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Work in pkg/io/hdf.py and only broaden if live references prove it necessary.\n4. Context: Root planning identified HDF as a direct owner surface.\n5. Acceptance Criteria: Submit a success summary with changed files and verification evidence."},
    {"id": "plan-parquet", "description": "Decompose parquet surface", "name": "team_planner", "deps": [], "scope_paths": ["pkg/io/parquet/"], "spec": "1. Goal: Decompose the remaining parquet owner surface.\n2. Environment: Use the current repository workspace and team runtime.\n3. Scope: Inspect pkg/io/parquet/ and keep child tasks inside confirmed parquet owners.\n4. Context: Root planning found parquet too broad for one atomic developer lane.\n5. Acceptance Criteria: Submit a valid child plan with concrete deps and scope_paths."},
    {"id": "val-root", "description": "Validate HDF and parquet", "name": "validator", "deps": ["dev-hdf", "plan-parquet"], "scope_paths": ["pkg/io/hdf.py", "pkg/io/parquet/"], "spec": "1. Goal: Run the terminal verification gate for this layer.\n2. Environment: Use the current repository workspace and available CI/test tools.\n3. Scope: Validate HDF and parquet changes covered by upstream tasks.\n4. Context: This validator depends on all terminal non-validator siblings.\n5. Acceptance Criteria: Submit a validation summary with commands run, results, and any remaining failures."}
  ],
  "output": "Root plan covers the confirmed HDF and parquet owner surfaces plus a terminal validator."
}
```
