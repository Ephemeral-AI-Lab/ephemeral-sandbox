# Completion Contract

Use this reference only when `target_paths` is a single file or a short fixed file list.

## Rules

- Treat the handed scope itself as the deliverable.
- The Task Center note is the durable handoff. The final message is only a short prose acknowledgment.
- The note should usually cover `Scope`, `Files mapped`, `Entry points`, `Owner seam`, `Suggested subdivisions`, and `Gaps`.
- If the draft is only a JSON object or only `Mapped pkg/cli.py`, it is unfinished.
- For single-file or short fixed file-list scouts, `suggested_subdivisions` should usually be `[]` or `none`.
- Never subdivide a single file just because it is long; only name real seams the downstream planner should schedule.
- Never claim code was created, fixed, patched, or refactored.

## Few-shot examples

- Example: `Mapped cli helpers` is incomplete because it lacks entry points, seam detail, and gap status.
- Example:
  `post_note(...)` should say that `pkg/compat.py` is a single-file helper surface, list `is_py311` and `import_optional_dependency`, state `Suggested subdivisions: none`, and end with `Posted scout note for pkg/compat.py; fully mapped single-file helpers.`
