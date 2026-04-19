# Widening And Runtime

Use this reference only when either condition is true:

1. You need to edit or create a file outside `scope_paths`.
2. The lane is runtime-owned and your evidence is still only syntax, LSP, or readback.

## Task/Goal

- You are deciding whether a widened edit belongs in this lane, or you are close to declaring success without runtime proof.

## Avoid

- If the scoped file is missing or disproved, must not widen by filename similarity alone. Do not hop to `pkg/foo_bar.py`, `pkg/_foo.py`, or another lookalike path by filename resemblance alone.
- If live evidence identifies a different production owner, missing module, compatibility shim, re-export module, or import bridge outside `scope_paths`, decide whether it is one adjacent production owner for the same bug. If yes, you may widen deliberately; if it changes ownership materially, submit a failure with the owner path and evidence so replanning can widen or resequence.
- If `scope_paths` itself names an absent module, shim, re-export module, or import bridge that came from a test import or collection error, production ownership evidence is required before writing. Otherwise submit a failure with the missing-path evidence.
- For path moves, file renames, compatibility shims, and re-export bridges, source and destination are separate ownership checks. An in-scope source file does not authorize an absent outside-scope destination path; if the destination is named only by tests or collection output, submit a failure before calling `daytona_move_file(...)`, `daytona_write_file(...)`, or `daytona_edit_file(...)`.
- If a verify command, CodeAct result, diagnostic, or collection output shows `ModuleNotFoundError`, `ImportError`, or a missing module outside `scope_paths`, classify it before writing. Continue only when live production evidence or the assigned objective proves the missing path is the intended repository surface; otherwise submit `submit_task_summary(type="request_replan", content=...)` with the command output.
- A similar in-scope compatibility module is not provenance for an absent private shim. Do not create, rename, move, or re-export `pkg/_compat.py` from a test import just because `pkg/compat.py` exists.
- Before calling `daytona_write_file(...)` or `daytona_edit_file(...)`, compare the target path to `scope_paths`. If it is outside scope, continue only after an explicit widened-edit decision; the advisory warning is not a runtime hard gate.
- Must not use warning/config overrides, blank `addopts`, or alternate pytest config as proof while normal startup is red.
- Do not skip, xfail, or rewrite the verify file just to make the benchmark look green.

## Workflow

- Must treat `scope_paths` as the default edit surface, compose with live sibling edits on widened files, and keep widened edits to one adjacent supporting owner surface for the same bug.
- Before a widened write, classify it: adjacent support for the same scoped owner may proceed; a missing module, compatibility shim, re-export, import bridge, or different owner may proceed only when live production evidence shows it is the intended repository surface. Otherwise report it with `submit_task_summary(type="request_replan", content=...)`.
- Before a widened move or rename, classify both endpoints; do not let an in-scope source file launder an outside-scope destination into the lane.
- For new files, `scope_paths` does not override provenance. If the only reason for the new path is a failing test import, fail for replanning instead of creating the file.
- If a delete/move tool returns an error, do not retry the same `daytona_delete_file(...)` or `daytona_move_file(...)` call and do not route around it through CodeAct or git; submit the tool error for replanning.
- Must treat failing tests and verify commands as evidence first, not automatic test ownership, and must not report success on a runtime-owned lane until one assigned runtime verification command passes.
- If the exact verify command fails before the named target collects, or a shared import/runtime-control problem fires first, keep that shared chain red until it is repaired or reverted.
- If the fault is ambient drift, including root or OS permission semantics that invalidate a test setup, stop and surface that mismatch instead of editing tests or improvising installs.
- If `daytona_edit_file` returns `verification-surface write allowed in advisory mode`, revert that test edit and widen only to the adjacent production/import chain that owns the failure.
- If any Daytona mutation returns an `outside write_scope` warning, treat it as coordination evidence: refresh notes if needed, avoid unrelated widening, and include the widened path and rationale in the terminal summary. If it proves the task needs a different owner, multiple owners, sequencing, or test-file authorization, submit `submit_task_summary(type="request_replan", content=...)`.
- If any Daytona mutation returns `verification-surface write allowed`, avoid or revert the test edit unless the task explicitly owns a test-only bug.
- If any runtime output names an outside-scope missing import or collection blocker, use the same widened-edit decision process even without a Daytona write warning.

## Expected Outcome

- Widening stays adjacent, justified, and anchored to one runtime-owned failing surface; a missing outside-scope owner is allowed only when it is a real production surface, otherwise it becomes replan evidence.
