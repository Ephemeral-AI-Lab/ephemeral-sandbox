---
name: sweevo-project-context
description: Stable SWE-EVO benchmark rules shared by planner, scout, developer, validator, and replanner agents.
---

# SWE-EVO Project Context

Use this skill only for stable benchmark policy. Treat the prompt, payload, live checkout, and named tests as the contract for the current run.

## Shared rules

- Must treat the live sandbox checkout as the source of truth. Must treat named `FAIL_TO_PASS`, `PASS_TO_PASS`, and grading commands as authoritative.
- Must report a missing named test or node as `benchmark_surface_mismatch`. Must not label a missing transitive import, helper, or adjacent production module as `benchmark_surface_mismatch`.
- Must keep commands repo-root-relative. Prefer direct `daytona_codeact(command="...", timeout=N)` for repo commands; use `daytona_codeact(code="...")` only for multi-step runtime that truly needs Python helpers. Never prepend guessed `cd /testbed`, `cd /workspace`, `cd /home/user`, use Python process wrappers, or append stdout/stderr capture plumbing such as `2>&1` or `2>/dev/null`.
- Must treat `daytona_codeact` as runtime-only on team lanes. Never use it for file mutations through `sed -i`, `tee`, output redirects, shell mutation commands, `rm`, `mv`, or inline Python writes; use `daytona_edit_file`, `daytona_write_file`, `daytona_rename_symbol`, `daytona_delete_file`, or `daytona_move_file`. All of them commit through the OCC-gated base-hash path; shell `rm`/`mv` would bypass it and are blocked.
- Must not use `daytona_codeact` for source inspection. Avoid `cat`, `sed -n`, `grep`/`rg`, `head`/`tail`/`nl`, Python file reads, and source introspection; use notes and CI first, then `daytona_read_file` or `daytona_grep`.
- Must fix repository code, not the ambient environment. Never rely on ad hoc package installs as the benchmark fix.
- Must keep roles separate, preserve exact file paths and exact pytest node ids when they are known, and trust live file state over cached briefs or old reasoning.
- Must treat benchmark test files as failure evidence first, not default implementation ownership, but must not create planner/scout ownership tasks whose scope is benchmark-test archaeology unless the prompt explicitly makes tests the owner surface.
- Must treat test-file writes as off-policy unless the prompt explicitly assigns a test-only bug. Prefer production fixes; if only a test edit seems viable, report the blocker for replanning.
- Must not derive an exact production file from benchmark filename resemblance alone, including `tests/test_foo.py -> pkg/foo.py` or public/private compat-name swaps without live import or note evidence.
- Must treat a structure-only sighting of sibling files as boundary evidence, not exact-owner confirmation, after a scout disproves a file or marks a directory tests-only.
- Must treat collection or import failures before the named target loads as still-red verification, not as a reason to trim the scope.

## Coordination redesign focus

- Must treat `docs/architecture/team-coordination.md` as the design intent for this benchmark.
- Must keep shared context in the Task Center: scouts post durable notes directly, developers and validators rely on Task Center auto-notes plus terminal submissions. Use `read_task_note(...)` for scout findings and dependency context, `read_task_note(scope="sibling", ...)` for sibling activity and conflict checking.
- Must use `read_task_note(paths=[...])` before opening source files, before launching duplicate scouts, and after every surprising verification failure; an empty note read is useful evidence, not a blocker.
- Must treat scope-change notifications and `task_center_changed_since()` as freshness signals. Refresh with `read_task_note(...)` before committing, verifying, or replanning on a drifting surface.
- These workflow rules are prompt/playbook obligations, not runtime guardrails. Do not wait for a tool error to enforce them; self-correct or submit failure evidence when a lane has gone off policy.
- Must keep `scope_paths` as soft coordination hints, not hard filesystem ownership bans.
- Must treat any advisory outside-scope write as a tainted packet and hand it to replan instead of claiming success from that run.

## Planning and execution emphasis

- Must keep fresh roots live-first: one narrow production anchor, then at least one scout wave before root plan JSON.
- Must split direct owner leaves early and leave unresolved or broad surfaces expandable. Never hide residual work behind placeholder lanes or one catch-all developer.
- Must start developer and validator execution lanes with `read_task_note(paths=[...])` before opening files or reproducing, even when the note set may be empty.
- Must start developer and validator runtime work from the exact failing command or exact named failure target.
- Must prefer Task Center notes, exact runtime evidence, and CI symbol tools over raw file reads on ready owner lanes. Use `daytona_read_file(...)` only after notes plus CI identify a narrow line range or a CI result needs local confirmation.
- Must not spend a ready leaf's opening moves reading benchmark tests when scout notes and exact runtime already name the owned seam.
- Must report exact failing ids and exact snippets. Never explain failures away.
- Must prefer recovery quality over perfect first-pass planning: validator evidence plus one live owner confirmation is enough to replan.

## Observability

- Must use `.ephemeralos/benchmark-logs/` as supporting evidence for runtime, coordination, checkpoint, and scoped-path notification behavior.
- Must prefer structured evidence that shows prompt/completion/total tokens, tool usage and limits, note flow, checkpoint lineage, and replans when those logs exist.
- Never let logs outrank the live workspace, current test output, or current Task Center state.
