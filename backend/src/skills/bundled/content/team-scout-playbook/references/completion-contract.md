# Completion Contract

Use this reference only when `target_paths` is a single file or a short fixed file list.

## Task/Goal

- The scout scope is a single file or a short fixed list and you are preparing the handoff.

## Avoid

- Never subdivide a single file just because it is long; only name real seams the downstream planner should schedule.
- Never claim code was created, fixed, patched, or refactored.

## Workflow

- Must keep the handed scope itself as the deliverable.
- The Task Center note is the durable handoff. Make exactly one `submit_task_note(...)` call with non-empty `content`; do not put the handoff only in visible prose.
- If the tool result returns and a final response is required, reply only `Posted.` and do not repeat the findings.
- The note should usually cover `Scope`, `Files mapped`, `Entry points`, `Owner seam`, `Suggested subdivisions`, and `Gaps`.
- If the draft is only a JSON object or only `Mapped pkg/cli.py`, it is unfinished.
- If the draft is assistant text with no `submit_task_note(...)` call, it is unfinished.
- For single-file or short fixed file-list scouts, `suggested_subdivisions` should usually be `[]` or `none`.

## Expected Outcome

- The scout handoff is short, durable, scoped exactly to the handed file set, and stored through `submit_task_note(...)`.
