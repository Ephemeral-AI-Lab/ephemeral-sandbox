# Exploration Script

Use this reference only on fresh benchmark roots or any turn that still lacks clear ownership.

## Workflow

1. Must keep the first-turn exploration script explicit and live-evidence-first.
2. Start with exactly one narrow `ci_workspace_structure(path=...)` on the deepest shared production boundary already implied by the prompt.
3. Use `ci_query_symbols(...)`, `ci_query_references(...)`, `ci_hover(...)`, or `ci_diagnostics(...)` only to refine likely owners from that anchor.
4. If the first anchor is empty, or `ci_status()` reports `initialized=false`, stop exact-file guessing immediately. This is a cold-CI branch.
5. On cold CI, keep unresolved work on stable directories/packages and let scouts confirm exact files. Failing benchmark tests remain evidence only.
6. If more than one owner slice is still unresolved after the first anchor, the next planning action must be a scout wave, not final DAG synthesis.
7. On repeated work, if one canonical owner is already exact and same-run reuse is empty, call `check_exploration_memory(paths=[...])` before relaunching that scout.
8. If a first-wave guessed exact path returns `File not found`, delete that leaf and wait for the scout note. Do not start a replacement ownership search mid-wave; keep the last confirmed parent boundary until note review.
9. After the wave, `read_notes(scope_paths=[...])`; if `context_changed_since()` or a scope-change warning says the layer moved, refresh only stale slices.
10. Stop exploring once the current layer can name ready work plus residual boundaries.

```json
{
  "good": {
    "anchor": "dask/dataframe/io",
    "cold_ci_wave": [
      "dask/dataframe/io/hdf.py",
      "dask/dataframe/io/json.py",
      "dask/dataframe/io/parquet/",
      "dask/dataframe/groupby.py"
    ]
  },
  "bad": {
    "extra_anchors_before_wave": ["dask/dataframe", "dask"],
    "guessed_exact_path": "dask/dataframe/utils_dataframe.py",
    "test_as_owner": "dask/dataframe/io/tests/test_parquet.py"
  }
}
```

## Rules

- Never map a benchmark cluster to a production file solely because the names look similar.
- Never overwrite any earlier brainstorm alias in the first-wave ledger without live evidence.
- Delete any brainstorm alias as soon as live evidence disproves it.
- Delete any earlier `pkg/dataframe/utils_dataframe.py` brainstorm as soon as live evidence disproves it.
- delete any earlier `pkg/dataframe/utils_dataframe.py` brainstorm once live evidence disproves it.
- Never open with root-wide CI queries or a broad first anchor when the prompt already points at a deeper production area.
- Never use more than one scope path as the first anchor or stack multiple first anchors before the wave.
- Never spend first-wave explorers on benchmark test files when a plausible production owner exists.
- Never spend a first-wave explorer entirely on benchmark test files; keep them literal in task prose or broaden to the last confirmed production package.
- Never guess missing production files from test names, keep a disproven alias in the first-wave ledger, or name an exact production file absent from live CI or explorer notes.
- Never derive `pkg/foo.py`, `pkg/foo_bar.py`, or a private compat module from a benchmark filename like `tests/test_foo.py` without live owner evidence.
- Never react to one missing guessed leaf by opening a new structure pass mid-wave; delete the leaf, keep the confirmed parent boundary, and wait for note review.
- Never use a later `ci_workspace_structure(...)` sibling listing as proof that a disproved leaf or tests-only directory now belongs to `pkg/utils.py`, `pkg/config.py`, or another nearby exact file; keep the last confirmed parent boundary broad until live symbol/import/note evidence says otherwise.
- Never bundle unrelated owner slices or the whole first-wave ledger into one explorer.
- Never start loading decomposition or plan-json references while the first explorer wave still has unlaunched exact-file slices.
- Never turn benchmark test filename tokens into nested directories, inserted path segments, or composite production files absent from live evidence.
- Never create a separate root task just to describe a benchmark mismatch; carry confirmed owners forward and drop disproved leaves.

Example scout launch shape:
`run_subagent(agent_name="scout", input={"target_paths":["pkg/io/parquet"]}, task_note="map the parquet owner surface")`
