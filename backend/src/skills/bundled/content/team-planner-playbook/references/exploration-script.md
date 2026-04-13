# Exploration Script

Use this reference only on fresh benchmark roots or any turn that still lacks clear ownership.

## Workflow

1. Start with exactly one narrow `ci_workspace_structure(path=...)` on the deepest shared production boundary already implied by the prompt.
2. Use `ci_query_symbols(...)`, `ci_query_references(...)`, `ci_hover(...)`, or `ci_diagnostics(...)` only to refine likely owners from that anchor.
3. If the first anchor is empty, or `ci_status()` reports `initialized=false`, stop exact-file guessing immediately. This is a cold-CI branch.
4. On cold CI, keep unresolved work on stable directories/packages and let scouts confirm exact files. Failing benchmark tests remain evidence only.
5. If more than one owner slice is still unresolved after the first anchor, the next planning action must be a scout wave, not another anchor or final DAG synthesis.
6. On repeated work, if one canonical owner is already exact and same-run reuse is empty, call `check_exploration_memory(paths=[...])` before relaunching that scout.
7. After the wave, `read_notes(scope_paths=[...])`; if `context_changed_since()` or a scope-change warning says the layer moved, refresh only stale slices.
8. Stop exploring once the current layer can name ready work plus residual boundaries.

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

- Never open with root-wide CI queries or a broad first anchor when the prompt already points at a deeper production area.
- Never use more than one scope path as the first anchor or stack multiple first anchors before the wave.
- Never spend first-wave explorers on benchmark test files when a plausible production owner exists.
- Never guess missing production files from test names, keep a disproven alias in the first-wave ledger, or name an exact production file absent from live CI or explorer notes.
- Never bundle unrelated owner slices or the whole first-wave ledger into one explorer.
- Never start loading decomposition or plan-json references while the first explorer wave still has unlaunched exact-file slices.
- Never turn benchmark test filename tokens into nested directories, inserted path segments, or composite production files absent from live evidence.
