---
name: sweevo-project-context
description: Stable SWE-EVO benchmark rules shared by planner, developer, validator, and replanner agents.
---

# SWE-EVO Project Context

Use this skill only for stable benchmark policy. Treat the prompt, payload, live checkout, and named tests as the contract for the current run.

## Shared rules

- Must treat the live sandbox checkout as the source of truth.
- Must treat named `FAIL_TO_PASS`, `PASS_TO_PASS`, and grading commands as authoritative.
- Must report a missing named test or node as `benchmark_surface_mismatch`. Must not guess a replacement node, file, or symbol.
- Must not label a missing transitive import, helper, or adjacent production module as `benchmark_surface_mismatch`.
- Must keep commands repo-root-relative. Never prepend guessed `cd /workspace`, `cd /home/user`, or similar wrappers.
- Must fix repository code, not the ambient environment. Never rely on ad hoc package installs as the benchmark fix.
- Must keep roles separate, preserve exact file paths and exact pytest node ids when they are known, and trust live file state over cached briefs or old reasoning.
- Must treat benchmark test files as failure evidence first, not default implementation ownership.
- Must keep benchmark test files and pytest node ids literal in task prose or retry targets, but must not create planner/scout ownership tasks whose scope is benchmark-test archaeology unless the prompt explicitly makes tests the owner surface.
- Must not derive an exact production file from benchmark filename resemblance alone, including `tests/test_foo.py -> pkg/foo.py` or public/private compat-name swaps without live import or note evidence.
- Must treat a structure-only sighting of sibling files as boundary evidence, not exact-owner confirmation, after a scout disproves a file or marks a directory tests-only.
- Must treat collection or import failures before the named target loads as still-red verification, not as a reason to trim the scope.

## Coordination redesign focus

- Must treat `docs/architecture/plan-a-team-coordination-redesign.md` as the design intent for this benchmark.
- Must keep shared context in the Task Center: scouts, developers, and validators post durable notes; planners, developers, validators, and replanners reuse them with `read_notes(...)`.
- Must use `check_exploration_memory(paths=[...])` only after same-run notes are insufficient and the scope is already exact.
- Must treat scope-change notifications and `context_changed_since()` as freshness signals. Refresh with `read_notes(...)` before committing, verifying, or replanning on a drifting surface.
- Must keep `scope_paths` as soft coordination hints, not hard filesystem ownership bans.
- Must treat any advisory outside-scope write as a tainted packet and hand it to replan instead of claiming success from that run.

## Planning and execution emphasis

- Must keep fresh roots live-first: one narrow production anchor, then at least one scout wave before root plan JSON.
- Must split direct owner leaves early and leave unresolved or broad surfaces expandable. Never hide residual work behind placeholder lanes or one catch-all developer.
- Must start developer and validator runtime work from the exact failing command or exact named retry target.
- Must use the CI toolkit for live ownership evidence and the context toolkit for coordination evidence.
- Must report exact failing ids and exact snippets. Never explain failures away.
- Must prefer recovery quality over perfect first-pass planning: validator evidence plus one live owner confirmation is enough to replan.

## Observability

- Must use `.ephemeralos/benchmark-logs/` as supporting evidence for runtime, coordination, retry, checkpoint, and scoped-path notification behavior.
- Must prefer structured evidence that shows prompt/completion/total tokens, tool usage and limits, note flow, checkpoint lineage, retries, and replans when those logs exist.
- Never let logs outrank the live workspace, current test output, or current Task Center state.
