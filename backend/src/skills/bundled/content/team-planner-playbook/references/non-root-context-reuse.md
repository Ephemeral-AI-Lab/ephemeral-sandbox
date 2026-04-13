# Non-Root Context Reuse

Use this reference only on child planning turns or prompts with `## Scoped Expansion`.

## Workflow

1. Must reuse inherited notes and known owner boundaries before fresh exploration.
2. Must reuse existing Task Center notes via `read_notes(scope_paths=[...])` before checking cross-run cache or opening a new scout; parent `bg_*` ids are not child-turn handles.
3. Must call `read_notes(...)` first when you need a live same-run freshness check on one inherited owner slice.
4. After `read_notes(...)`, must use at most one live owner confirmation step on the one unresolved owner when siblings are already mapped.
5. Must emit direct lanes for already-mapped siblings instead of replanning the whole repository.

## Rules

- Must keep exact file paths until a live artifact confirms an exact node id.
- Must recover real live filenames instead of guessed aliases.
- Must deepen the DAG only for the unresolved branch. Do not serially re-plan already-settled siblings.
- Must trust current live evidence over older inherited notes when they disagree.
- Must use `check_exploration_memory` only after same-run inherited/shared context is insufficient and the exact owner scope is already named.
- Must keep direct ready lanes ready even when one residual branch still needs a child planner.
- Must emit a direct developer lane when the child turn already owns one exact production file and inherited evidence already names its internal families. Add direct developer leaves plus at most one terminal validator; do not emit another child planner for that same file, and do not reopen same-file scouts unless live coherence drift erased the family split.
- Never reopen a broad workspace scan if the parent already handed down the relevant slice boundary, and never probe parent background ids such as `bg_2`; reuse existing Task Center notes or refresh via `read_notes(...)` first.
- Never invent replacement nodes, replacement files, or broad substitute ownership from a stale test name.

## Few-shot example

- Example: parent already narrowed the residual slice to `pkg/utils.py` plus `tests/test_utils.py`.
  Emit a direct `developer` lane and, if needed, one sibling `validator` lane for that exact slice.
  Do not emit another `team_planner` child for the same single-file residual.
- Example: parent hands down one scout for `pkg/groupby.py`, and the child task is to split `cov`, `unique`, and `value_counts`.
  Use `read_notes(scope_paths=["pkg/groupby.py"])`, then the inherited notes plus live symbol lookup on `pkg/groupby.py` to emit three developer lanes and one validator whose `deps` list all three developers.
  Do not relaunch region scouts or emit another `team_planner` for `pkg/groupby.py` once the family split is already named.
- Example: parent hands down parquet scope, but Task Center already includes fresh scout notes for `pkg/io/parquet`.
  Reuse those notes via `read_notes(scope_paths=["pkg/io/parquet"])` if freshness is unclear.
  Do not call `check_background_progress(task_id="bg_2")` inside the child turn; that id belongs to the parent planner, not this layer.
