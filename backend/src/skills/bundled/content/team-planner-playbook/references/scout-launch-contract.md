# Scout Launch Contract
Use this reference immediately before the first scout wave or whenever scout launching stalls.

## Task/Goal

- You are about to launch or recover the first useful scout wave.

## Avoid

- Never pass prompt mode to `scout`.
- Do not jump to `check_background_progress(task_id="bg_3")` on an inferred id or before the useful wave is fully queued.
- Never call `check_background_progress(...)` just to satisfy a ritual; use it only when live status will change whether you continue, wait, cancel, or read notes.
- Never launch explorers for benchmark tests when a plausible production owner already exists.
- Never launch explorers on `*/tests/test_*.py` or grouped benchmark test files; keep those test paths literal in task prose or broaden to the nearest live production package.
- Never launch a scout with mixed benchmark-test and production `target_paths`; keep the benchmark test path in task prose and scout only the live production scope.
- Never pass `*/tests/*`, `test_*.py`, or an unconfirmed/missing test-derived path in scout `target_paths` when production owners exist; put that evidence in `task_note` or task prose.
- Never use a scout to locate or correct a benchmark test path mismatch; keep the literal test path in task prose and scout the production owner path instead.
- Never pass an exact file to a scout after a file-symbol query found no indexed symbols and workspace structure shows a live directory or nested files for that same owner family.
- Never derive explorer `target_paths` by copying failing test paths after the anchor already exposed the production owner.
- Never bundle unrelated exact files or the whole first-wave ledger into one explorer.
- Never launch a second explorer on the same slice in the same turn just because the first one is still running.
- Never launch an explorer whose entire target stays inside one exact file already covered by existing Task Center notes or same-turn work.
- Never delay the first explorer wave behind extra sibling structure passes once the current anchor already exposed the needed owner files.
- Never start loading decomposition references or progress checks while the first useful wave is only partially launched.
- Never check background progress on an inferred id that was never returned by `run_subagent`.
- Never check or wait on a scout id again after a status payload says `delivered`, `Posted.`, `[ALREADY_COMPLETED]`, or `[NO TASKS RUNNING]`.

## Workflow

1. Scrub `target_paths` first: every entry should be a live production owner file/directory unless tests are explicitly the owner surface; do not use scouts to repair benchmark test paths.
2. Must call `run_subagent(agent_name="scout", input={"target_paths": [...]}, task_note="...")` exactly.
3. Give each scout one unresolved owner slice, not a bundle of unrelated files.
4. Queue the whole useful wave before any progress check, wait, or reaction to early output.
5. After the wave is queued, keep making foreground progress; use at most one progress check only if live status changes the next planning action.
6. After the wave, planners must `read_task_note(paths=[...])` with default scope. Notes from `run_subagent` scouts live on the current planner task; do not use `scope="sibling"` for them.
7. When any scout status is terminal (`delivered`, `Posted.`, `[ALREADY_COMPLETED]`, or `[NO TASKS RUNNING]`), remove that id from your active background set and read its notes; do not poll or wait on it again.
8. Reuse existing Task Center notes when the same scope already has coverage; same-turn overlap is a reuse signal, not a cue to relaunch the same explorer.
9. If cold CI blocked exact-file confirmation, or an exact file is disproved by structure that shows a directory/nested files instead, launch the nearest stable production boundary instead of synthesizing or preserving a guessed exact path.
10. Record the exact returned `task_id` for every scout and use only those literal ids in progress checks or waits.
11. After the wave, if `context_changed_since()` or a scope-change warning says the layer moved, refresh notes before shaping the DAG.

## Expected Outcome

- The full useful scout wave is queued once, tracked by literal task ids, and followed by note review before DAG shaping.
