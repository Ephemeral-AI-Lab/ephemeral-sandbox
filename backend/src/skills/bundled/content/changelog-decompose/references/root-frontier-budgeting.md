# Root Frontier Budgeting — SWE-EVO Planning Reference

Use this reference when a release bump has more changelog bullets than the benchmark can profitably work on in the first execution frontier.

This reference does not add a new architecture. The default shape is still:

1. decompose owned implementation lanes
2. add only real dependencies
3. finish with one verification task

Use this file only to decide which owned lanes belong in the first frontier.

---

## Goal

Build a root graph that spends the first worker slots on the highest-value work:

- benchmark-critical FAIL_TO_PASS root causes
- fixture or environment work that is strictly required to unblock those fixes
- nothing else

If concurrency is effectively capped at 2, a four-lane first frontier usually means two lanes are waiting anyway. Those waiting slots should not belong to low-evidence support chores.

When the root goal or project context already includes a parsed changelog list, benchmark test-file summary, or shallow repo/module map, treat that brief as the starting point for this budgeting pass. Use root discovery to confirm ownership boundaries, not to rediscover the same changelog text or repo skeleton.

---

## First-Frontier Rule

For small and medium SWE-EVO instances:

- keep the initial ready frontier to **at most 2 benchmark-critical implementation lanes**
- every first-frontier lane should be justified by one of:
  - a distinct FAIL_TO_PASS behavior cluster
  - a required shared fixture or environment unlocker for those FAIL_TO_PASS tests

If a changelog bullet does not meet one of those tests, it should not become an independent root lane.

---

## Large-Changelog Cluster Frontier

When the root graph consists of module clusters (from Step 0 triage for changelogs with 50+ items), apply frontier budgeting at the cluster level:

- first-frontier clusters: those with FAIL_TO_PASS evidence (typically 2-4 clusters)
- remaining clusters: fold them into one optional downstream expandable follow-up macro (for example `secondary-release-followups`) only when residual owned work remains after folding into primary clusters or verification
- each cluster is itself expandable — child coordinators apply the normal first-frontier rule within their scope

This creates a two-level frontier:

1. Root level: which clusters run first
2. Cluster level: which lanes within a cluster run first (handled by child coordinator)

### Hard Cap on First-Frontier Clusters

Even when multiple clusters have FAIL_TO_PASS evidence, cap the first-frontier at **3 expandable cluster macros** to avoid provider throttling:

- If 1-3 clusters have FAIL_TO_PASS evidence → all are first-frontier
- If 4+ clusters have FAIL_TO_PASS evidence → pick the 3 with the most FAIL_TO_PASS tests as first-frontier; remaining go into the optional residual follow-up macro with `depends_on` the first 3

Why: Each expandable first-frontier cluster triggers a child coordinator expansion, which itself makes multiple provider API calls. With 5+ concurrent expansions, provider rate limits cause cascading failures. The marginal value of the 4th concurrent cluster is lower than the risk of throttling all workers.

---

## Where Remaining Changelog Bullets Go

When a changelog bullet is real but not yet justified as first-frontier work, choose one:

1. Fold it into the neighboring owned lane if it touches the same production surface.
2. Put it in one **downstream expandable macro** such as `secondary-release-followups`, with a wave-based hint.
3. Attach the check to final verification if the code may already satisfy the release note.

Do **not** create one root lane per uncertain bullet.
Do **not** emit a follow-up macro just because some bullets are left over on paper. If every parsed bullet can already be assigned to a primary lane, bridge task, or verification, omit the follow-up macro entirely.

---

## Good vs Bad Root Graphs

### Good

```text
[params-root-cause] ──┐
[ignore-root-cause] ──┼── [secondary-release-followups ↻] ── [verification]
```

- first frontier is entirely benchmark-critical
- non-critical follow-ups are delayed until the primary fixes land
- the follow-up macro can expand later into atomic child work if still needed

### Bad

```text
[params-fix]
[ignore-tests]
[cli-help]
[oss-azure-fixtures]
[verification]
```

- root frontier mixes critical fixes with secondary chores
- capped concurrency means low-value work competes with benchmark-critical work
- no place to defer “maybe already implemented” or broad support tasks

---

## Expandable Follow-Up Macro Pattern

When secondary work is still needed after isolating the primary failure clusters, prefer a single downstream expandable macro:

```text
task_id: secondary-release-followups
expandable: true
depends_on: [primary-lane-a, primary-lane-b]
expansion_hint:
  Wave 1: confirm remaining release bullets against real source files and tests.
  Wave 2: emit only the still-missing production or fixture updates.
  Wave 3: collapse residual checks into final verification.
```

This keeps the root graph compact while preserving room for real work if the release note is not already satisfied.

If that downstream macro still mixes CLI work, behavior fixes, and fixture changes across multiple directories, it should remain **expandable** at the root. Do not compress it into one large atomic “everything else” lane.
If the residual set shrinks to zero after folding neighboring work or verification checks, delete the macro instead of keeping an empty placeholder.

---

## Full Coverage Contract

Root frontier budgeting is allowed to defer work, but it is not allowed to lose work.

Before emitting the root graph, build a compact coverage ledger for the parsed changelog bullets:

- every parsed bullet must be assigned to exactly one destination:
  - primary implementation lane
  - cross-cutting bridge lane
  - downstream follow-up macro
  - verification-only check
- no parsed bullet may remain unassigned
- if a bullet is verification-only, the planner should be able to explain why discovery suggests the code may already satisfy it

The ledger can stay lightweight. It does not need one root task per bullet. It does need complete coverage.

Good:

```text
CL-001 -> array-root-cause
CL-002 -> dataframe-root-cause
CL-003 -> verification-only
CL-004 -> secondary-release-followups
```

Bad:

```text
Primary clusters: array, dataframe
Remaining bullets: many
```

The second form is not a coverage plan; it is only a frontier summary.

---

## Required Evidence Before Creating a Root Lane

A root implementation lane should have all of:

- owned production or fixture files, not only changelog prose
- at least one concrete symbol or entry point from repo inspection and concrete file reading
- a reason it must run before verification or before another lane can proceed

If any of those are missing, do not emit that lane at the root.

---

## DVC 1.1.7 → 1.1.8 Example

Primary behavior clusters:

- `test_params_with_false_values` → `dvc/dependency/param.py` → `ParamsDependency.fill_values`
- `test_ignore_file_in_parent_path` and related ignore behaviors → `dvc/ignore.py` plus coupled ignore tests

Likely secondary bullets:

- CLI subcommand help
- OSS/Azure fixture changes

Unless the planner reads code that proves those bullets are missing and required now, they should be folded into a downstream follow-up macro or verification instead of taking first-frontier worker slots.
