---
name: sweevo-project-context
description: Stable SWE-EVO benchmark constraints and project context template for coordinator and worker agents operating on software evolution tasks.
---

# SWE-EVO Project Context

This skill carries the stable benchmark policy for SWE-EVO runs. Instance-specific facts such as the repo, exact FAIL_TO_PASS targets, PASS_TO_PASS guardrails, grading command, and frontier cap come from the current prompt or WorkItem payload.

When benchmark prose, release notes, or changelog bullets disagree with the live checkout and named tests, the live checkout plus the named tests win.

---

## Shared benchmark constraints

- **Source of truth is the current sandbox checkout.** The SWE-EVO test patch is already applied in the sandbox for this run. Treat the working tree, the named FAIL_TO_PASS targets, the PASS_TO_PASS guardrails, and the grading command as the benchmark contract.
- **Changelog prose is background context only.** Do not treat release notes or version-transition prose as the implementation checklist.
- **Fix the repository, not the ambient environment.** Do not rely on ad hoc `pip install`, `conda install`, `uv add`, or other sandbox-only environment mutation as the benchmark fix. If dependency metadata is part of the solution, land it in the repo-managed manifest or lockfile.
- **Plan -> Execute -> Validate.** Planner decomposes, developer edits, validator verifies. Do not collapse those phases into one role.
- **Background work stays backgrounded.** After launching a scout or other background task, keep working other ready surfaces, use `check_background_progress` for spot checks, and wait only when that result is the remaining blocker.
- **Progress checks are per fresh scout.** If you launch a new scout and later want to wait on it, inspect that specific task with `check_background_progress` first. Do not spawn a batch of fresh scouts and then immediately wait on them.
- **Live tooling beats cached context.** Atlas briefs, shared briefings, and planner hints are useful snapshots. On any conflict, trust live code intelligence and live sandbox reads.

---

## Planner rules for SWE-EVO

- Use the FAIL_TO_PASS list as reproduction signals, not as a reason to scout giant test files just to restate known failures.
- Once one likely owner file or subsystem is known, stop changelog/version archaeology. Hand off the symptom, likely owner, exact reproduction target, and verification target.
- Do not treat a dependency pin or `pyproject.toml` entry as the root cause from the root planner just because the changelog mentions a version bump. A manifest bump is only a planner-owned lane when the task is explicitly packaging-related or a developer later confirms the repo manifest is the real fix from live evidence.
- Split disjoint owner clusters into separate source-owned execution lanes. Do not collapse unrelated modules into one omnibus developer task just because they appear in the same release-note block.
- If one file is large but still the likely owner, a bounded single-file scout is valid. If that still leaves several independent regions, emit a narrowed child planner instead of forcing a flat root plan.
- Parent and sibling exploration lanes must stay disjoint. Do not reopen a slice already owned by a scout or child planner unless new evidence invalidates the boundary.
- Once two source-owner scouts have returned enough structure to name the likely implementation surfaces, the root planner should stop scouting and emit the plan. Additional confirmation belongs to developer or validator lanes, not to the root planner.

---

## Developer rules for SWE-EVO

- Start from the exact named failing test or a faithful reproduction lifted directly from it.
- Planner diagnoses are hypotheses until the current failing output confirms them.
- After one targeted reproduction plus one or two focused code reads identify the deciding function or branch, edit immediately. Do not spend the attempt on repeated ad hoc probes.
- If the exact retry target is already green in the sandbox, stop debugging and report that result; let the validator spend the one broader regression check.
- Fix production code first. Do not edit tests, snapshots, or benchmark harness files unless the WorkItem explicitly assigns them.

---

## Validator rules for SWE-EVO

- Start with the exact retry target(s) named by the payload or benchmark context.
- After the exact retry target passes, spend at most one broader same-surface regression command unless the payload explicitly requires more.
- If the exact retry target fails, report that failure immediately with exact test ids, exit code, and a short verbatim error snippet.
- The benchmark harness will run the full grading command after the team phase. Do not spend validator budget duplicating broad redundant suites by default.

---

## Observability and state

- Usage totals, model breakdowns, checkpoints, and retry metadata are part of the benchmark evidence. Prefer the latest healthy checkpoint when deciding what to resume.
- When reporting a blocker, include the exact command, exit code, failing test ids, and likely owner surface so replanning can stay surgical.
