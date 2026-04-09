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

## Retry loop

- **First attempt should be fresh.** Start benchmark investigation from a fresh sandbox so you can trust the first failure signal.
- **After local runtime or skill fixes, prefer sandbox reuse.** Re-run with a stable `sandbox_name` so the harness can reuse the latest healthy prepared sandbox instead of rebuilding the image each time.
- **Reuse the latest healthy evidence.** Checkpoints, scout artifacts, token totals, and validator evidence are part of the retry surface. Do not restart cold if the existing sandbox is healthy and the repo can be reset in place.

---

## Planner rules for SWE-EVO

- On large benchmark roots, spend the first exploration pass on 2-3 disjoint source-owner scouts rather than one long serial hypothesis lane.
- If the first scout wave comes back partial or still leaves several disjoint owner hypotheses alive, launch another disjoint scout wave or a narrowed child planner. Do not freeze the root plan just because the first wave already ran.
- Once two scout waves or roughly 25 planner tool calls have already gone into the same root benchmark surface, the default next step is the plan. A third wave needs a genuinely new disjoint owner cluster, not a deeper read of the same mapped clusters.
- Use the FAIL_TO_PASS list as reproduction signals, not as a reason to scout giant test files just to restate known failures.
- When the failure surface is broad, cluster by likely production owner and guardrail surface first. A hundred failing test IDs in one module still count as one source-owner lane, not a hundred planner tasks.
- Once one likely owner file or subsystem is known, stop changelog/version archaeology. Hand off the symptom, likely owner, exact reproduction target, and verification target.
- If the next planner thought is "I need to understand the actual test failures" inside a cluster that is already source-owner complete, stop and hand that cluster to a developer or validator. Exact runtime mismatch confirmation belongs downstream.
- Do not treat a dependency pin or `pyproject.toml` entry as the root cause from the root planner just because the changelog mentions a version bump. A manifest bump is only a planner-owned lane when the task is explicitly packaging-related or a developer later confirms the repo manifest is the real fix from live evidence.
- Root planners must not spend CI turns on dependency-name or import archaeology (`pydantic_core`, package pins, installed versions, lockfiles) once concrete source owners are known. If version drift is still plausible, pass it to the developer lane as a hypothesis tied to an exact reproduction target.
- Once source-owner scouts exist, do not open new manifest or giant-test scouts. Remaining uncertainty belongs to a developer or validator lane unless source ownership is still ambiguous.
- Split disjoint owner clusters into separate source-owned execution lanes. Do not collapse unrelated modules into one omnibus developer task just because they appear in the same release-note block.
- If one file is large but still the likely owner, a bounded single-file scout is valid. If that still leaves several independent regions, emit a narrowed child planner instead of forcing a flat root plan.
- Parent and sibling exploration lanes must stay disjoint. Do not reopen a slice already owned by a scout or child planner unless new evidence invalidates the boundary.
- While scouts are running, keep the planner moving on other uncovered branches, shared-context reuse, and plan-shape reasoning. Wait only when a scout result becomes the remaining blocker.
- Once the returned scout evidence is sufficient to name the likely implementation surfaces and direct validation surfaces, the root planner should stop scouting and emit the plan. This may happen after one wave or several; additional confirmation belongs to developer or validator lanes, not to the root planner.
- If the planner receives a budget warning, the next assistant message must be the final plan JSON. Do not spend the remaining budget checking background progress or reopening hypotheses.
- Treat duplicate-scout rejections and background wait protocol errors as stop-and-plan signals. Reuse the gathered evidence instead of retrying the same exploration pattern.
- A repeated `WAIT_REQUIRES_PROGRESS_CHECK` or repeated whole-batch wait on the same benchmark wave is evidence that the planner should finish the plan, not evidence that another planner-side deep-dive is needed.

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
