# Expandable Guidance

Mark a task `expandable: true` when any of the following are true:

- It spans multiple subsystems or unrelated directories.
- It bundles multiple changelog assignments (independent bullets, buckets, or release-note chunks) under one description.
- It maps to two or more functional areas, even within one file cluster, and still needs sequencing.
- It references more than one explicit path root (for example `backend/`, `frontend/`, `agents/`, `services/`), and one worker cannot finish it in one pass.
- It likely touches multiple files and directories such that one worker cannot finish it in one pass.
- It is a cross-cutting pass or follow-up bucket that requires another planning iteration.
- The exact touched files are not yet known.
- The work likely needs another planning pass before implementation.
- The task combines implementation and cross-cutting validation follow-up across different areas.
- The synthesized hotspot is narrow, but the likely production fix still spills into sibling implementation files outside the candidate lane's declared ownership.
- One worker would need to diagnose across neighboring production files before ownership is concrete.

- If a task includes multiple changelog assignments and the execution steps are still unclear, mark it `expandable: true`.
- Do not default a grounded lane to `expandable: true` just because one or two branch-local sibling files may still need inspection.
- If one worker can plausibly finish the lane inside one owned slice and the next child plan would only restate implementation steps, keep it `expandable: false`.
- Default to `expandable: true` only when the remaining uncertainty still spans multiple plausible execution surfaces, multiple ownership clusters, or a next submitted level that can immediately fan out into 2+ disjoint worker leaves.
- Bias toward `expandable: true` for large changelogs when a task would otherwise be a coarse-grained bucket.
- If you are unsure whether a task should stay whole, split it first and mark only the remaining cross-cutting task as expandable.
- If a task looks like a root-to-leaf change (`backend`, `frontend`, `services`, `tests`, `docs`) with two or more roots, force `expandable: true`.
- Do not use `expandable: true` to justify a repo-external placeholder task or a release-wide test/docs umbrella without a concrete in-repo ownership anchor.
- Prefer implementation lanes with local validation or docs follow-up over one giant expandable cleanup bucket for the whole release.
- At root depth, use `expandable: true` to continue one concrete explored slice or hotspot family, not to create a broad parent-path umbrella over several already-known sibling slices.
- At root depth, keep a grounded one-cluster behavior fix as a leaf when one worker can plausibly finish it directly and child planning would only restate the same implementation steps.
- If a candidate leaf would require editing neighboring production files outside its declared owned cluster, keep it expandable or split the hotspot before submission.

- For release-driven tasks, if a task description references two or more explicit changelog IDs (for example `CL-001`, `CL-002`) and spans more than one file set, it is not atomic:
  - split into focused atomic tasks first, or
  - keep it as `expandable: true` with a concrete expansion plan.

Leave `expandable: false` only when the task is execution-sized:

- One worker can complete it directly.
- The change surface is narrow and concrete.
- The touched files are mostly known up front.
- The probable production fix stays inside the declared owned cluster plus local validation files.
- The validation target is clear.

Examples:

- Atomic:
  - "Add a deprecation shim for the remaining old parameter aliases in one module"
  - "Fix one concrete indexing bug in a single implementation file"
- Expandable:
  - "Handle all remaining API deprecations across the codebase"
  - "Adjust tests for the whole release"
  - "Implement compatibility updates across several core modules"
