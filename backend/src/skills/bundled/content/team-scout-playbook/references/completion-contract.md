# Completion Contract

Use this reference only when `target_paths` is a single file or a short fixed file list.

## Task/Goal

- The scout scope is a single file or a short fixed list and you are preparing the handoff.

## Avoid

- Never subdivide a single file just because it is long; only name real seams the downstream planner should schedule.
- Never turn an off-policy benchmark test target into path correction work; report that the planner should scout the production owner path instead.
- Never prescribe skipping, xfail-marking, rewriting, or reconfiguring benchmark tests, benchmark harness files, or pytest configuration; report dependency, optional-extra, environment, or production-owner evidence as a hypothesis or gap instead.
- Never claim code was created, fixed, patched, or refactored.

## Workflow

- Must keep the handed scope itself as the deliverable.
- For a single-file or short fixed file-list scout, use at most one file-path `ci_query_symbol(...)` per assigned path after the note reads. If those queries return definitions for every assigned path, the next tool must be `submit_file_notes(...)`.
- Missing exact-target gate: if an exact-file bootstrap query returns no definitions, or the bootstrap evidence shows the exact file is missing or replaced by a directory/package boundary, the next tool must be `submit_file_notes(...)`; do not call `ci_workspace_structure(...)`, run `ci_query_symbol(...)` on nearby helper names like `read_*` or `to_*`, or inspect adjacent files/directories to reverse-engineer a replacement owner.
- If exact-file bootstrap definitions exist, do not call `ci_workspace_structure(...)` or extra symbol/test queries just to elaborate the same seam; record any unresolved question under `Gaps`.
- Context hypotheses do not widen the handed file set. If `target_paths` is `["pkg/groupby.py"]`, do not query `pkg/core.py` or sibling owners just because the context speculates about them; record the adjacent path as an unresolved gap instead.
- The Task Center handoff is durable and batched. Make exactly one `submit_file_notes(...)` call with one note item per assigned target path and non-empty `content` in each item; do not put the handoff only in visible prose.
- If the tool result returns and a final response is required, reply only `Posted.` and do not repeat the findings.
- The note should usually cover `Scope`, `Files mapped`, `Entry points`, `Owner seam`, `Suggested subdivisions`, and `Gaps`.
- For no-symbol exact files whose owner family is a live directory or nested files, the `Gaps` section should say the exact file should not be used as `scope_paths`; list live directory/nested-file evidence separately.
- For a missing exact file, the note should say the scout recorded zero coverage for that exact path and did not search for a nearby replacement owner.
- For benchmark test target paths that are not explicit test-only owner surfaces, the `Gaps` section should say the target path is off-policy and scouts should map production owners instead.
- If the draft is only a JSON object or only `Mapped pkg/cli.py`, it is unfinished.
- If the draft is assistant text with no `submit_file_notes(...)` call, it is unfinished.
- For single-file or short fixed file-list scouts, `suggested_subdivisions` should usually be `[]` or `none`.

## Expected Outcome

- The scout handoff is short, durable, scoped exactly to the handed file set, and stored through `submit_file_notes(...)`.
