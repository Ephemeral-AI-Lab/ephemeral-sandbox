# Exploration Script

Use this reference only on fresh benchmark roots or any turn that still lacks clear ownership.

## Workflow

1. Must start with one narrow `ci_workspace_structure(...)` pass on the nearest likely production directory or package.
2. Must follow with `ci_scoped_status(scope_paths=[...])` on one exact existing production path from that listing.
3. Must use code intelligence to seed likely owners from live symbols, package structure, and the scoped packet before naming scout slices.
4. If more than one owner slice is still unresolved after the anchor, the next planning action must be a scout wave, not final DAG synthesis.
5. Must launch scouts only after that live anchor exists.
6. Must keep each scout on one distinct unresolved owner slice.
7. Must stop exploring once the current plan layer can name ready work plus residual boundaries.

## Scout fanout strategy

1. Must fan out by distinct production-owner slices, not by raw failing-test count.
2. The first wave should match the real unresolved frontier. Often that is 3-6 scouts, but it may be 1 when only one slice is genuinely unclear.
3. If more slices are unresolved than the current layer can responsibly carry, must launch the most diagnostic disjoint subset now and park the rest behind child planners or a later wave.
4. Must keep every scout narrow enough that it answers one ownership question.
5. Must launch another wave only when the first wave returns partial ownership and several disjoint owner slices are still unresolved.
6. Must stop fanout as soon as the next plan layer can name the dominant owner slices, residual boundaries, and at least one ready leaf lane.

## Few-shot examples

If the live anchor shows failures that plausibly map to `pkg/io/`, `pkg/schema/`, and `pkg/compat/`, the first wave should be three scouts:

- Scout 1: `target_paths=["pkg/io"]`
- Scout 2: `target_paths=["pkg/schema"]`
- Scout 3: `target_paths=["pkg/compat"]`

Must not split that into one scout per failing test file.
Must not collapse those three owner slices into one omnibus scout.
Must stop after that wave if it already identifies the dominant owner slice and the residual boundary.

If the anchor points to `pkg/groupby.py`, `pkg/io/parquet/`, `pkg/io/json.py`, `pkg/config.py`, and `pkg/compat.py`, do not collapse the last three into one "misc" planner just because they are small.
Scout them separately until live evidence shows that two or more really converge on the same production owner.

If the live anchor confirms `pkg/io/hdf.py` as the dominant owner and a child branch still needs deeper mapping inside `pkg/io/parquet/`, emit one direct developer lane for HDF and park parquet behind a child planner.
Do not hold the ready HDF lane hostage just because parquet is still exploratory.

If the anchor shows several plausible owners but no scout has run yet, do not load final-plan references and do not draft JSON from reasoning alone.
The next step must look like `run_subagent(agent_name="scout", input={"target_paths":["pkg/io/parquet"]}, task_note="Map parquet owner slice")`, repeated for the other unresolved slices.
Only after those scout briefs return may the planner load decomposition guidance and finalize the DAG.

## Rules

- Never open with root-wide CI queries.
- Never spend first-wave scouts on benchmark test files when a plausible production owner exists.
- Never guess missing production files from test names.
- Never bundle unrelated owner slices into one scout just to reduce lane count.
- Never sit on an anchor-only picture for a long reasoning pass when unresolved owner slices still exist; scout immediately.
- Never map a benchmark cluster to a production file solely because the names look similar.
- Never use Atlas as a substitute for the first same-run scout wave.
- Never keep scouting after owner sufficiency is reached.
- Treat duplicate-scout rejection, repeated wait protocol errors, and budget warnings as stop-and-plan signals.
