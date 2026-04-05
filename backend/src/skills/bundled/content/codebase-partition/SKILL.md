---
name: codebase-partition
description: Guides the codebase-partitioner agent to identify semantically meaningful exploration regions from workspace structure and symbol data.
---

# Codebase Partition

You are partitioning a codebase into exploration regions. Each region will be assigned to a parallel explorer agent. Your partitioning quality directly determines exploration coverage.

---

## Partitioning Strategy

If you need a reminder of the live tool contract for this run, load
`references/runtime-tool-surface.md`. Treat the runtime overlay as authoritative
over examples in this skill.

### Step 1 — Assess Scale

Use `query_ci_status()` to determine:
- Total files indexed → dictates region count
- Symbol index readiness → determines if you can use `query_symbols()`

| Codebase Size | Target Regions | Strategy |
|---|---|---|
| < 20 files | 1 | Single region at root `.` |
| 20–200 files | 2–3 | Top-level directory split |
| 200–2000 files | Scale with goal-relevant dirs | One region per relevant domain, capped by explorer count |
| 2000+ files | Narrow to goal-relevant only | Only regions with keyword matches; fall back to largest dirs if none match |

Region count scales with goal relevance, not file count. Prefer fewer focused regions over many broad ones. When in doubt, match the number of goal-relevant directories rather than targeting a fixed count.

### Step 2 — Map Structure

Use `query_workspace_structure(".", 2)` to get the directory tree with file counts. This is your primary partitioning input.

Identify:
- **Source directories**: where implementation lives (src/, lib/, app/, pkg/)
- **Test directories**: where tests live (tests/, test/, __tests__/, spec/)
- **Config root**: setup files, manifests, CI configs
- **Documentation**: docs/, README files

### Step 3 — Score by Goal Relevance

For each candidate directory, assess relevance to the goal:

1. **Direct naming match**: directory name contains goal keywords → high priority
2. **Import proximity**: if symbol index is ready, use `query_symbols()` on 2-3 candidate dirs to check if they contain symbols mentioned in the goal
3. **Size proportionality**: larger directories need their own region; tiny dirs can merge

### Implementation-First Weighting

When the goal is to implement, fix, or verify code:

- **Prefer production source directories first**. Source modules and their nearby tests outrank docs, examples, benchmarks, and packaging metadata.
- **Use FAIL_TO_PASS or test-target clues as anchors** when they are present in the goal context. A source directory mentioned by a failing test should outrank a docs directory mentioned by the release notes.
- **Do not spend a region on docs/examples/changelog text by default**. Documentation should stay context-only unless the goal explicitly requires documentation edits or there is unused exploration budget after the main code-owning regions are covered.
- **If one package root dominates the implementation surface, split that source tree before splitting docs**. A second source subpackage is usually a better use of budget than a standalone docs region.
- **Include docs only when they are explicit deliverables**. Changelog, deployment, or install docs should become their own region only when the goal directly requires documentation changes or when there is spare exploration budget after the main code paths are covered.
- **Treat compatibility and deprecation work as source-first**. Library version bumps, API deprecations, and behavioral fixes should bias toward the owning implementation modules plus the tests that exercise them.

### Large Release / Benchmark Triage

When the goal context names a large release, benchmark, or changelog-heavy task:

- Extract concrete file-path clues from the goal context before you partition.
- If the context explicitly cites 2+ distinct source roots or test-owned roots, prefer those roots over a single package-wide region.
- Convert cited test paths into their owning source roots when possible. Example: `pkg/foo/tests/test_bar.py` usually maps to `pkg/foo`.
- Do not return one broad package root when the context already proves multiple narrower source subtrees.
- Use the package root only as a secondary context lane when at least two narrower implementation lanes already exist.

### Single-Package Repositories

When one package root dominates the repo (for example `src/`, `lib/`, or a single top-level package like `dask/`), do not stop at that top-level directory if the goal spans multiple subdomains.

- If the dominant package has multiple meaningful subpackages, split at the next level down: examples include `src/auth`, `src/api`, `lib/runtime`, or `dask/dataframe`.
- Use FAIL_TO_PASS file families and changelog prefixes to justify multiple source regions inside that package root.
- Keep the package root itself as a context region only when shared foundations or root-level modules appear relevant; otherwise prefer the narrower subpackages.
- A single broad region for an implementation-heavy, multi-cluster goal is usually under-partitioned and will starve exploration breadth.

### Step 4 — Emit Regions

Each region should:
- Cover a **cohesive domain** (not arbitrary file splits)
- Have a **focused exploration directive** in the `focus` field
- Use `priority: "high"` for goal-relevant regions, `"normal"` for context regions

---

## Anti-Patterns

- **Don't split by file type** (all .py in one region, all .ts in another) — split by domain
- **Don't create 1-file regions** — merge small related directories
- **Don't ignore test directories** — pair test regions with their source regions in the focus field
- **Don't create overlapping regions** — each file should belong to exactly one region
- **Don't spend your full budget** — 2-3 tool calls should suffice for partitioning

---

## Output Contract

This is a helper skill. The enclosing phase prompt or runtime schema is authoritative.

- If the active phase prompt already declares a JSON object schema, follow that schema exactly.
- Use this skill only to improve how you choose and describe regions.
- Do not invent extra top-level keys unless the active phase schema explicitly allows them.

If no enclosing phase schema is provided, fall back to returning a JSON array of region objects:

```json
[
    {"path": "backend/src", "focus": "API endpoints and business logic for <goal>", "depth": 3, "priority": "high"},
    {"path": "frontend/src", "focus": "UI components and state management", "depth": 3, "priority": "normal"},
    {"path": "shared", "focus": "Shared types, utils, and configuration", "depth": 2, "priority": "normal"}
]
```

The `focus` field should be a specific directive, not a generic "explore this directory". Tell the explorer what to look for.
