# Macro Expansion And Atomic Ownership — Planning Reference

Use this reference when a release-evolution lane is too broad to stay atomic and must be emitted as `expandable=true`.

Expansion is a fallback, not the default. If the work can stay as a small owned lane with a clean dependency edge, do that and skip this file.

---

## Goal

Use macro expansion to preserve ownership, not to hide an unstructured bucket of leftover work.

An expandable lane is valid only when the parent already explains how the child coordinator should regroup the work into focused owned slices.

---

## What A Good Expandable Lane Looks Like

A strong expandable lane names one dominant subsystem family and already hints at the child split:

- the major child slices or clusters
- the owned production surfaces for each slice
- whether a scoped bridge or scoped verification task is likely needed

Good shapes:

- `Cluster parser recovery, AST edge cases, and import resolution fixes in src/compiler.`
- `Wave 1: split parser and serializer ownership. Wave 2: split CLI wiring and fixture updates.`
- `Cluster 1 (6 items): rolling/groupby API deprecations. Cluster 2 (4 items): dtype and output-shape compatibility fixes.`

Bad shapes:

- `Handle remaining follow-up work later.`
- `Expand this module into child tasks.`
- `Take care of miscellaneous release fixes after the main work lands.`

If the hint only says "remaining work" or "miscellaneous fixes," the child coordinator has no stable ownership boundary.

---

## Rules For Expandable Macros

1. An expandable macro should already name **2-4 child slices** using waves, bullets, numbered clusters, or an explicit ownership list.
2. Each child slice should map to a dominant production file, symbol family, or tightly coupled surface.
3. Do not group unrelated surfaces into one atomic child task just because they share a version bump.
4. If the macro still mixes multiple independent deliverables after discovery, keep the macro expandable and let the child coordinator split it again.
5. A downstream follow-up macro is acceptable only if its hint still names concrete child clusters. "Everything else" is not a cluster.
6. If the parent coverage ledger shows no residual owned work after folding into primary lanes or verification, delete the follow-up macro instead of expanding an empty bucket.

## Scoped Child Planning

When decomposing a macro below the root:

- use the parent hint as evidence about likely owned slices, not as a rigid checklist
- preserve explicit child ownership slices when the parent already named them
- split a named child only when it still hides multiple deliverables
- regroup long checklists into a few owned clusters instead of one atomic child per bullet

Default child sizing:

- prefer 2-4 child slices for ordinary macros
- allow 3-8 child slices only when the parent scope is clearly broad
- keep a child atomic only when you can name its dominant production file or symbol and its coupled tests or fixtures

If the child scope still spans multiple independent failure clusters, major directory families, or mixed production plus broad fixture work, keep that child expandable instead of flattening it.

## Remaining Depth

Always shape the child graph to fit the remaining expansion depth.

- If only one expansion level remains, emit atomic children only.
- If two expansion levels remain, use expandable children only for clearly broad clusters.
- Do not spend extra depth restating the same checklist at smaller granularity.

---

## Atomic Task Contract

Before a child slice stays atomic, the planner should be able to answer:

- What production file or symbol does this task own?
- Which tests or fixtures are coupled to that owned behavior?
- Why does no sibling task need to participate before this task can finish?

If those answers are fuzzy, the slice should remain expandable or be regrouped.

---

## Dependency Rules Inside A Macro

Only add sibling `depends_on` edges when one child slice truly needs another slice's artifact or API change.

Do not add child dependencies just because:

- two slices live in the same package
- two slices mention the same release theme
- two slices might eventually touch a shared wiring file

Use a scoped bridge task instead when the integration is real but downstream.

Discovery inside a scoped child plan should stay narrow: confirm only the
directories or symbols needed to defend the child ownership split, and do not
repeat broad root-level repo surveys once the scope is already known.

---

## Scoped Verification

When a child macro fans out into **3 or more atomic implementation slices**, strongly prefer a scoped verification task that depends on all of them if:

- the slices share warnings, API contracts, or compatibility behavior
- the macro owns one release-critical failure family
- the child coordinator can already name a focused regression command

Leave verification to the root only when the child slices are completely independent or the regression command is necessarily global.
