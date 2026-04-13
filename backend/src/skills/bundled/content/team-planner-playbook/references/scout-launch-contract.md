# Scout Launch Contract
Use this reference immediately before the first scout wave or whenever scout launching stalls.

## Launch workflow

1. Call `run_subagent(agent_name="scout", input={"target_paths":[...]}, task_note="...")` exactly.
2. Give each scout one unresolved owner slice, not a bundle of unrelated files.
3. Queue the whole useful wave before any progress check, wait, or reaction to early output.
4. Inspect fresh scouts with `check_background_progress(...)` before any `wait_for_background_task(...)`.
5. Scouts must `post_note(scope_paths=[...])`. After the wave, planners must `read_notes(scope_paths=[...])`.
6. Reuse existing Task Center notes when the same scope already has coverage; same-turn overlap is a reuse signal, not a relaunch signal.
7. If cold CI blocked exact-file confirmation, launch the nearest stable production boundary instead of synthesizing a guessed exact path.
8. Record the exact returned `task_id` for every scout and use only those literal ids in progress checks or waits.
9. After the wave, if `context_changed_since()` or a scope-change warning says the layer moved, refresh notes before shaping the DAG.

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
    {"target_paths": ["pkg/utils_dataframe.py"]},
    {"target_paths": ["pkg/io/hdf.py", "pkg/groupby.py"]}
  ]
}
```

## Rules

- Never pass prompt mode to `scout`.
- Never wait on a fresh or uninspected explorer before `check_background_progress(...)`.
- Never launch explorers for benchmark tests when a plausible production owner already exists.
- Never derive explorer `target_paths` by copying failing test paths after the anchor already exposed the production owner.
- Never bundle unrelated exact files or the whole first-wave ledger into one explorer.
- Never launch a second explorer on the same slice in the same turn just because the first one is still running.
- Never launch an explorer whose entire target stays inside one exact file already covered by existing Task Center notes or same-turn work.
- Never delay the first explorer wave behind extra sibling structure passes once the current anchor already exposed the needed owner files.
- Never start loading decomposition references or progress checks while the first useful wave is only partially launched.
- Never check background progress on an inferred id that was never returned by `run_subagent`.
- Never synthesize an explorer target by splitting a benchmark test filename into guessed directories or by naming an exact production file absent from live evidence.
