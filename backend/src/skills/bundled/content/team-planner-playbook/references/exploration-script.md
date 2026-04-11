# Exploration Script

Use this reference only on fresh benchmark roots or any turn that still lacks clear ownership.

## Workflow

1. Must start with one narrow `ci_workspace_structure(path=...)` pass on the deepest shared production directory or package already implied by the prompt failures.
2. Must follow with `ci_scoped_status(scope_paths=[...])` on exactly one existing production path from that listing.
3. Must use code intelligence to seed likely owners from live symbols, references, and the scoped packet before naming scout slices. After the first anchor, one focused `ci_query_symbols(...)` or `ci_query_references(...)` beats ad hoc reasoning.
4. Must translate benchmark failure evidence into production-owner slices before scout launch. Failing test paths stay evidence only.
5. If you catch yourself counting failing tests, guessing missing dependencies, checking benchmark test files, or listing source files to inspect before a scout wave, reset to the anchor instead of continuing that thread.
6. Any exact production file or package named in reasoning, scout input, or plan output must already exist in the current live workspace listing or scoped packet; otherwise keep the slice on the nearest existing production boundary.
7. If another failure family sits outside the current anchor, must branch through the nearest production directory or package for that family after the first anchor, not by widening the first anchor to cover everything at once.
8. If a similar-looking filename is absent from the live listing, keep that owner slice unresolved and scout the nearest existing production boundary instead of inventing a sibling file.
9. If more than one owner slice is still unresolved after the anchor, the next planning action must be a scout wave, not final DAG synthesis. On resumed or repeated work, if one canonical owner is already exact and same-run reuse is empty, use `atlas_lookup(...)` before spawning a duplicate scout.
10. After the first `ci_scoped_status(...)`, must not spend extra sibling `ci_workspace_structure(...)` passes on owners the prompt and current listings already exposed; scout them instead.
11. Once live evidence names an exact existing owner path, keep reusing that exact path in later reasoning, scout notes, and plan lanes unless a later live packet changes it.
12. Before loading decomposition or plan-shaping references, must launch the whole currently nameable first scout wave.
13. Must keep each scout on one distinct unresolved owner slice and stop exploring once the current plan layer can name ready work plus residual boundaries.

## Few-shot examples

- Example: the live anchor shows failures that plausibly map to `pkg/io/`, `pkg/schema/`, and `pkg/compat/`.
  Launch three scouts, one per owner slice; do not split them into one scout per failing test file or collapse them into one omnibus scout.
- Example: benchmark failures mention `pkg/io/tests/test_hdf.py`, `pkg/io/tests/test_parquet.py`, `pkg/tests/test_groupby.py`, `pkg/tests/test_cli.py`, `pkg/tests/test_config.py`, and `pkg/tests/test_compat.py`.
  Start with `ci_workspace_structure(path="pkg/io")`, then `ci_scoped_status(scope_paths=["pkg/io/hdf.py"])`, then scout `pkg/io/hdf.py`, `pkg/io/parquet/`, `pkg/groupby.py`, `pkg/cli.py`, `pkg/config.py`, and `pkg/compat.py`.
- Example: the anchor shows several plausible owners but no scout has run yet.
  The next step must look like `run_subagent(agent_name="scout", input={"target_paths":["pkg/io/parquet"]}, task_note="Map parquet owner slice")`, repeated for the other unresolved slices.
- Example: a resumed run already names `pkg/io/parquet/` from prior work, but the current run has no reusable scout ref for it.
  Re-anchor with live CI, then try `atlas_lookup(...)` for that canonical owner before launching another scout.
- Example: benchmark failures mention `pkg/tests/test_utils_dataframe.py`, but your current live listing has not shown `pkg/dataframe/utils.py` and still does not show `pkg/utils_dataframe.py` or `pkg/utils/dataframe.py`.
  Do not invent `pkg/utils/dataframe.py`, and do not shorten the family to `pkg/utils.py`. Branch through the nearest existing production package first, or reuse `pkg/dataframe/utils.py` only after a live listing proves it exists.

## Rules

- Never open with root-wide CI queries or a broad first anchor when the prompt already points at a deeper production area.
- Never call the first `ci_scoped_status(...)` with more than one scope path.
- Never spend first-wave scouts on benchmark test files or use a benchmark test file as a temporary `ci_scoped_status(...)` anchor when a plausible production owner exists.
- Never guess missing production files from test names or name an exact production file absent from the current live listing or scoped packet.
- Never bundle unrelated owner slices into one scout or keep adding sibling `ci_workspace_structure(...)` passes after the unresolved slices are already nameable.
- Never start loading decomposition or plan-json references while the first scout wave still has unlaunched exact-file slices.
- Never copy benchmark test paths or test directories into scout `target_paths` after the anchor exposed production owners for those failures.
- Never map a benchmark cluster to a production file solely because the names look similar.
- Never turn benchmark test filename tokens into nested directories or composite production files that were absent from live evidence.
- Never use Atlas as a substitute for the first same-run scout wave. Use it only after same-run reuse is exhausted and the owner scope is already exact.
- Treat duplicate-scout rejection, repeated wait protocol errors, and budget warnings as stop-and-plan signals.
