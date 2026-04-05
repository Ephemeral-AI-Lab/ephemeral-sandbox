# Large-Changelog Triage — Planning Reference

Use when the changelog for a release bump exceeds 50 items.

This reference is only a routing shortcut for very large changelogs. It does not add new mandatory planning layers beyond:

1. decompose owned work
2. add reliable dependencies
3. finish with verification

---

## Classification Heuristics

### Primary: Module prefix matching

Most large projects use consistent subsystem labels in changelog items:

| Pattern style | Example label | Example item |
| --- | --- | --- |
| explicit module prefix | `array:`, `storage:`, `cli:` | `storage: preserve schema ordering in parquet reader` |
| namespaced package path | `pkg.subpkg:` | `analytics.window: fix centered rolling bounds` |
| component tag in release note | `[api]`, `[planner]`, `[io]` | `[planner] avoid duplicate dependency edges` |

### Secondary: Directory name matching

When items lack explicit prefixes, match keywords against the shallow repo-structure survey:

- `tokenizer` -> likely `src/parser/` (grep for the term if uncertain)
- `parquet` -> likely `src/table/io/`
- `scheduler` -> likely `src/scheduler/`

### Fallback: Unclassified bucket

Items that match no prefix or directory keyword go into the `unclassified` bucket. This bucket is folded into the optional residual follow-up macro or split across related clusters during root graph construction.

---

## Cluster Sizing

| Cluster size | Action |
| --- | --- |
| <5 items | Merge into nearest neighbor cluster |
| 5-200 items | Good — one expandable macro |
| 200-500 items | Child coordinator emits 3-5 expandable subtasks (uses depth 1→2→3) |
| >500 items | If the cluster is one cohesive package family (e.g., `dask/dataframe/`), keep it as one expandable macro — the child coordinator will decompose further. If it spans multiple distinct sub-modules, check a slightly deeper repo-structure survey and consider splitting into 2-3 sub-clusters at root level. |

---

## Expansion Hint Format

Each cluster's `expansion_hint` should follow this structure:

```text
Cluster: dask.array (147 items)
Primary directory: dask/array/
FAIL_TO_PASS tests in this cluster:
- tests/test_array.py::test_rechunk_unknown
- tests/test_slicing.py::test_negative_step

Changelog items:
- array: fix rechunking with unknown dimensions (#1234)
- array: improve slicing with negative steps (#1235)
- array: add support for sparse output in map_blocks (#1236)
[... all 147 items ...]
```

For clusters of 200+ items, the expansion_hint may reach 5-10K tokens. This is acceptable — `build_scoped_project_context()` passes it through without truncation, and the child coordinator's context budget can accommodate this alongside discovery output.

## Coverage Ledger Requirement

Step 0 triage is not complete when you only know which clusters run first. It is complete when every parsed changelog item has a destination.

Before emitting the root graph, keep a compact ledger like:

```text
CL-001 -> parser-cluster
CL-002 -> table-io-cluster
CL-003 -> scheduler-cluster
CL-004 -> residual-followups
CL-005 -> verification-only
```

Rules:

- every parsed item must appear exactly once
- the residual follow-up macro is optional, not mandatory
- if all remaining items can be folded into primary clusters or verification, omit the residual follow-up macro
- if the residual follow-up macro exists, its expansion hint must name the residual child clusters or owned surfaces, not just "remaining work"

---

## Worked Example: Large Multi-Package Release

Given ~1800 changelog items and a shallow repo-structure survey returning:

```text
src/
  parser/
  planner/
  table/
  io/
  scheduler/
  compat/
  cli/
  tests/
```

Root triage produces:

| Cluster | Items | FAIL_TO_PASS? | Frontier |
| --- | --- | --- | --- |
| parser | ~280 | Yes (2 tests) | First |
| table/io | ~410 | Yes (4 tests) | First |
| scheduler | ~190 | Yes (1 tests) | First |
| cli | ~55 | No | Secondary |
| compat | ~120 | No | Secondary |
| deprecations | ~260 | No | Secondary |
| unclassified | ~485 | Maybe | Secondary |

Root graph:

```text
[parser-cluster ↻]    ──┐
[table-io-cluster ↻]  ──┼── [residual-followups ↻] ── [verification]
[scheduler-cluster ↻] ──┘
```

Depth budget for this example:

```text
depth 0: root triage -> 3 first-frontier clusters + optional residual follow-up macro + verification
depth 1: cluster child (for example, parser) -> 2-4 implementation lanes (some expandable)
depth 2: lane expansion → atomic subtasks
depth 3: atomic work execution
```

All within `_MAX_EXPANSION_DEPTH = 4`.

---

## Input Size Constraint

Step 0 triage assumes one-line items (~20 tokens each). For 2000 items that is ~40K tokens — fits alongside workspace structure and skill references in a 200K context window.

If the raw changelog has multi-paragraph item descriptions exceeding 50K tokens total, pre-truncate to one-line-per-item before the coordinator processes it. This is a data preparation step, not a coordinator concern.

---

## Anti-Patterns

1. **Deep-analyzing all items at root** — reading 20+ files to understand 2000 changelog items defeats the purpose of triage. The root coordinator should complete in 5-8 tool calls.

2. **One atomic task per item** — 2000 atomic tasks would overwhelm the execution engine. Group items into 5-15 cluster macros at root level.

3. **Semantic classification** — trying to understand what each item means rather than prefix-matching it to a subpackage. Triage is mechanical, not analytical.

4. **Sub-sub-clustering** — recursing Step 0 triage more than once hits the depth limit. Prefer wider fan-out (more expandable subtasks at depth 1) over deeper nesting (Step 0 at depth 1 producing Step 0 at depth 2).

5. **Mixing cluster granularity** — creating one cluster per PR or one cluster per file. Clusters should map to subpackage families surfaced by the shallow repo-structure survey.
