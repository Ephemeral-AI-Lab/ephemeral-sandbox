---
name: team-root-planner-playbook
description: Playbook for the root_planner agent. Analyze the user request, scout risk-bearing production ownership, then synthesize and submit a schema-valid root plan with submit_plan(...).
---

# Team Root Planner Playbook

Produce the root task DAG from the user request. Finish with exactly one `submit_plan(...)` call.

The root planner routes top-down. Identify owner families, delegate broad or clustered decomposition to child `team_planner` lanes, and reserve direct `developer` lanes for narrow exact-owner work.

## Workflow Map

| Stage | Purpose | Output contract |
| --- | --- | --- |
| 1. Analyze | Classify the request and build an owner ledger. | Clear owners, scout-required owners, unresolved owners, and verification evidence. |
| 2. Scout | Resolve unresolved or benchmark-risk production ownership. | Scout notes or explicit uncertainty for each launched target. |
| 3. Synthesize and submit | Convert evidence into a schema-valid same-payload DAG and emit the terminal payload. | One `submit_plan({ "new_tasks": [...] })` call and no later tools. |

Stage boundary: `load_skill_reference(...)` belongs only to Stage 3. Do not call it while the next action is still to analyze failing tests, decide whether to scout, launch scouts, wait for scouts, or read scout notes. The reference load is a one-way transition: after it, do not launch scouts, poll/wait/read scout notes, or make CI/workspace/symbol exploration calls.

Decision flow:

```text
User request
  -> [1. Analyze]
       classify intent, split evidence from production ownership,
       flag clustering, and build the owner ledger
  -> unresolved owners or benchmark-risk owner families?
       yes: [2. Scout] launch one scout per required production family,
            join the wave, read notes, and carry missing notes as uncertainty
       no: continue
  -> Stage 1 owner ledger complete and no scout still needed/running?
       no: stay in [1. Analyze] or [2. Scout]
       yes: [3. Synthesize and submit]
            load synthesize-and-submit, apply clustering + lane routing,
            draft tasks, run the checklist, and submit_plan
```

## Reference Map

Catalog only. Do not load references from this map during Stage 1 or Stage 2.

- `synthesize-and-submit`: clustering and lane selection, coverage/evidence rules, `submit_plan` contract plus `NewTaskSpec` field table, valid and invalid payload examples, task-spec examples for `developer`, `team_planner`, and `validator`, and dependency DAG examples with rationale. Load in Stage 3 before drafting any `submit_plan(...)` payload.

Post-load rule: after `load_skill(...)`, build the Stage 1 owner ledger first. A `load_skill_reference(...)` call immediately after `load_skill(...)` is invalid unless a prior assistant action already completed Stage 1 and any required Stage 2 scout work.

Reference load gate: trigger -> owner ledger exists, every `scout_required` or unresolved production owner has either completed scout evidence or explicit uncertainty, every scout wave has been joined, and every available scout note has been read; required action -> load `synthesize-and-submit` as the first Stage 3 action via `load_skill_reference(skill_name="team-root-planner-playbook", reference_name="synthesize-and-submit")` and then only draft/check/submit; failure signal -> `load_skill_reference(..., reference_name="synthesize-and-submit")` appears before Analyze/Scout evidence, in the same action that first analyzes failing tests, immediately after `load_skill(...)`, while scouts are running, or before later scout/CI exploration.

Pre-load checklist — answer all five Yes in visible reasoning before the first `load_skill_reference(...)` call:

1. Is the owner ledger `{ clear, scout_required, unresolved, evidence }` written out as text in this turn (not merely a list of failing test ids)?
2. For every slice in `scout_required` or `unresolved`, has a scout been launched OR the slice been explicitly carried as uncertainty?
3. Have all launched scouts joined (`check_background_progress` shows none running) and every available note been read?
4. Is the next productive step drafting `submit_plan(...)` rather than more analyze or scout work?
5. Can the rest of the run finish with drafting/checklist/`submit_plan(...)` only, with no later scout, note-read, CI, workspace, or symbol exploration?

Any No means the load is premature. Stay in Stage 1 or Stage 2 and do not load the reference yet.

## Workflow Details

### 1. Analyze

Build an owner ledger before routing. The root planner has no parent, deps, or Task Center graph context to load.

- Classify intent as bugfix, refactor, feature, migration, benchmark, or mixed.
- Raise a clustering flag when the request spans many failing tests, several production families, or an engine/dtype/format/API matrix under one broad subsystem.
- Split verification evidence from production ownership. Failing tests, benchmark ids, and verification commands go into child specs; concrete production files, directories, and symbols become owner slices.
- Scout coverage gate: trigger -> a benchmark/fail-to-pass/migration/compatibility cluster has several production families, broad owner paths, or matrix-shaped coverage; required action -> put each family in `scout_required` even when its first-pass owner label looks clear; failure signal -> only the unknown families are scouted while clear-looking families go to Stage 3 with no live scout evidence. Example: ✓ read-API scout + storage-engine scout + CLI scout + config scout; ✗ two scouts for six independent clusters because four looked clear.
- Boundary-probe limit: trigger -> you want live structure help before scouting; required action -> use at most one targeted `ci_workspace_structure` or `ci_query_symbol` call to confirm the single most decision-relevant package/file boundary or named symbol owner, then carry remaining uncertainty into scouts; failure signal -> a broad triad such as `dask/dataframe/io`, `dask/dataframe`, and `dask` before the scout wave. Skip this only when the user already named exact production paths; benchmark test paths are verification evidence, not owner proof.
- Output `{ clear, scout_required, unresolved, evidence }`, where every requested slice is classified as a clear production owner, scout-required production owner, unresolved production owner, or verification evidence.

Do not patch, validate, or read production files yourself. Do not guess owners from benchmark imports, filename similarity, or directory listings. Do not treat test edits, skips, xfails, or pytest reconfiguration as production ownership.

### 2. Scout

Skip this stage only when the owner ledger has no `scout_required` or unresolved production slices.

- Launch one scout per `scout_required` or unresolved production owner family. Use `run_subagent(agent_name="scout", input={"target_paths": ["<one production owner path>"], "context": "..."})`; for a package family, use one directory path rather than several sibling files.
- Benchmark/fail-to-pass clustering trigger -> launch the first scout wave as parallel per-family calls before any polling -> failure signal: one broad scout bundles unrelated families. Example: ✓ HDF scout + parquet scout + CLI scout + config scout in the same wave; ✗ one scout with HDF, parquet, groupby, CLI, and config targets.
- Single-family payload gate: trigger -> one scout input would include target paths from two ledger rows or unrelated failing clusters, even when they share a package or both look small; required action -> split them into separate `run_subagent` calls in the same wave; failure signal -> one background task mixes HDF with JSON, groupby with utils, or CLI with config/compat ownership.
- Small-cluster merge gate: trigger -> you want to combine two narrow owner families because each one is small, lower-priority, or nearby in the benchmark list; required action -> keep separate scouts anyway and let Stage 3 merge evidence later if appropriate; failure signal -> one scout bundles groupby with JSON, config with compatibility, or any other unrelated "small" families to save a scout call.
- Same-test-family split gate: trigger -> one failing test area points at two production families such as config vs CLI, package init vs command entrypoint, or any other sibling owners; required action -> keep them as separate ledger rows and launch separate scouts with one target path each; failure signal -> one scout `target_paths` list contains both owners because they came from the same test file or benchmark cluster.
- Stable-boundary gate: trigger -> a scout family is unresolved between guessed exact files, a guessed exact file and an ancestor directory, or the draft `target_paths` would append a broad fallback path to concrete owner files; required action -> choose one stable production boundary path in `target_paths` and move guessed files or adjacent-row fallbacks into `context`; failure signal -> one scout uses both `pkg/__init__.py` and `pkg/_version.py`, both `dask/dataframe/groupby.py` and `dask/dataframe`, both `dask/dataframe/io/parquet.py` and `dask/dataframe/io`, or repo-root fallback like `dask` next to concrete owner files.
- Keep `target_paths` production-only. Put tests, `test_*.py`, benchmark harnesses, verification paths, missing test-derived files, failing ids, skipped variants, optional-dependency errors, and verification commands in scout `context`.
- Fire every useful scout before polling. Use `check_background_progress(task_id="all")` and `wait_for_background_task(task_id="all")` until no scout is running.
- Cancel only a halted, blocked, off-scope, or twice-stale scout with `cancel_background_task(task_id=id)`, then carry that slice as explicit uncertainty.
- Read every available scout note with `read_file_note(file_path=...)` and forward notes plus uncertainty to Stage 3.

If any candidate target matches `*/tests/*`, `test_*.py`, a benchmark harness, or a verification-only path, do not launch a scout on it — move that path into scout `context` and keep `target_paths` production-only.

Scout only owner information that changes root routing. Do not scout to confirm exact files already named by the user unless that owner belongs to a broad benchmark or matrix cluster; then scout the family boundary, not the test path or benchmark harness.

### 3. Synthesize and submit

Enter this stage only after Stage 1 output exists and Stage 2 is complete or explicitly skipped because no unresolved production owners remain. The reference load is the stage transition; if you are still building the owner ledger or might need exploration, do not load it yet. Once the pre-load checklist in the Reference Map is all Yes, load `synthesize-and-submit` as the first Stage 3 action, then proceed without further scout, note-read, CI, workspace, or symbol exploration.

- Use the reference's clustering, lane selection, coverage/evidence, dependency DAG, and submission rules to route each slice to `developer`, `team_planner`, or `validator`.
- Name-field lock: if your synthesis calls a slice expandable, clustered, broad, multi-family, matrix-shaped, unresolved, mixed, or not atomic, the task's `name` must be `team_planner`, never `developer`.
- Draft each task with `id`, `name`, `deps`, `scope_paths`, and a `spec` containing `1. Goal:`, `2. Task Details:`, and `3. Acceptance Criteria:`.
- Before submit, audit every `developer` task: it must have passed every atomic test in the reference, and its own `Goal` / `Task Details` must not describe the same slice with any expandable signal.
- If a new production owner slice would require exploration after the reference load, keep it unresolved and route it to a child `team_planner` or scoped diagnostic task; do not call scouts or CI/workspace/symbol tools after the Stage 3 transition.
- Run the reference's Final Checklist, then emit `submit_plan({ "new_tasks": [...] })` as the final assistant action. Submit top-level `new_tasks` only: no summary, output, parent ids, trailing prose, or later tool calls.

Every named failing cluster must be owned by a repair/decomposition task or handed to a child `team_planner`; a terminal validator is never the owner of an unassigned cluster. Do not route expandable multi-family work as a catch-all `developer`, decompose expandable work inline at the root, route narrow atomic work through `team_planner`, insert `deps` just to serialize independent work or keep scopes disjoint, preserve scout suggestions to edit/skip/xfail/rewrite/reconfigure tests unless the user asked for test repair, or include `scout` or `team_replanner` in `new_tasks`.
