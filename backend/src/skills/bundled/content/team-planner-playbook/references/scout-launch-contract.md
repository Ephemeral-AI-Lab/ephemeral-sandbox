# Scout Launch Contract
Use this reference immediately before the first scout wave or whenever scout launching stalls.

## Launch workflow

1. Must call `run_subagent(agent_name="scout", input={"target_paths": [...]}, task_note="...")` exactly.
2. Give each scout one unresolved owner slice, not a bundle of unrelated files.
3. Queue the whole useful wave before any progress check, wait, or reaction to early output.
4. Must finish queuing the useful wave before any progress check or reaction to early scout output.
5. Inspect fresh scouts with `check_background_progress(...)` before any `wait_for_background_task(...)`.
6. Scouts post findings to the Task Center after their work completes. After the wave, planners must `read_notes(scope_paths=[...])`.
7. Reuse existing Task Center notes when the same scope already has coverage; same-turn overlap is a reuse signal, not a cue to relaunch the same explorer.
8. If cold CI blocked exact-file confirmation, overwrite any stale guessed aliases in the first-wave ledger and launch the nearest stable production boundary instead of synthesizing a guessed exact path.
9. Record the exact returned `task_id` for every scout and use only those literal ids in progress checks or waits.
10. After the wave, if `context_changed_since()` or a scope-change warning says the layer moved, refresh notes before shaping the DAG.
11. delete any earlier `pkg/dataframe/utils_dataframe.py` brainstorm once live evidence disproves it.

```json
{
  "wave": [
    {"target_paths": ["pkg/io/hdf.py"]},
    {"target_paths": ["pkg/io/parquet/"]},
    {"target_paths": ["pkg/groupby.py"]},
    {"target_paths": ["pkg/config.py"]}
  ],
  "anti_patterns": [
    {"target_paths": ["tests/test_utils_dataframe.py"]},
    {"target_paths": ["tests/test_cli.py", "tests/test_compatibility.py"]},
    {"target_paths": ["tests/test_utils_dataframe.py", "pkg/io/utils.py"]},
    {"target_paths": ["pkg/io/hdf.py", "pkg/groupby.py"]}
  ]
}
```

## Rules

- Never pass prompt mode to `scout`.
- Do not jump to `check_background_progress(task_id="bg_3")` on an inferred id or before the useful wave is fully queued.
- Never wait on a fresh or uninspected explorer before `check_background_progress(...)`.
- Never launch explorers for benchmark tests when a plausible production owner already exists.
- Never launch explorers on `*/tests/test_*.py` or grouped benchmark test files; keep those test paths literal in task prose or broaden to the nearest live production package.
- Never launch a scout with mixed benchmark-test and production `target_paths`; keep the benchmark test path in task prose and scout only the live production scope.
- Never derive explorer `target_paths` by copying failing test paths after the anchor already exposed the production owner.
- Never bundle unrelated exact files or the whole first-wave ledger into one explorer.
- Never launch a second explorer on the same slice in the same turn just because the first one is still running.
- Never launch an explorer whose entire target stays inside one exact file already covered by existing Task Center notes or same-turn work.
- Never delay the first explorer wave behind extra sibling structure passes once the current anchor already exposed the needed owner files.
- Never start loading decomposition references or progress checks while the first useful wave is only partially launched.
- Never check background progress on an inferred id that was never returned by `run_subagent`.
- Never synthesize an explorer target by splitting a benchmark test filename into guessed directories or by naming an exact production file absent from live evidence.
