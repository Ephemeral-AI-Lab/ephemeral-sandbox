---
name: codebase-synthesize
description: Guides the codebase-synthesizer agent to merge parallel explorer reports into a unified architectural analysis with cross-cutting insights.
---

# Codebase Synthesis

You receive N explorer reports covering different regions of a codebase. Your job is to produce a unified analysis that reveals architectural patterns no single explorer could see.

---

## Synthesis Process

If you need a reminder of the live tool contract for this run, load
`references/runtime-tool-surface.md`. Treat the runtime overlay as authoritative
over examples in this skill.

### Step 1 — Catalog Inputs

For each explorer report, note:
- Region path and exploration status (completed / partial / failed / timeout)
- Number of symbols discovered
- Cross-references pointing to other regions

Discard reports with status `failed` or `timeout` that have no useful data. Note them as exploration gaps.

### Step 2 — Merge Structure

Build a unified directory overview:
- One paragraph per region summarizing its purpose and architecture
- Do NOT concatenate raw structure strings — synthesize them into a coherent narrative
- Note the overall architecture pattern (monolith, microservices, monorepo, modular)

### Step 3 — Extract Cross-Cutting Concerns

Scan all reports for patterns that appear in 2+ regions:

- **Shared base classes**: e.g. `BaseModel` imported by both `api/` and `workers/`
- **Common utilities**: e.g. `utils/logging.py` used across all regions
- **Configuration patterns**: e.g. env var loading via the same `config` module
- **Error handling**: e.g. shared exception hierarchy or error middleware
- **Testing patterns**: e.g. shared fixtures, common test base classes

Each cross-cutting concern should name the specific files/symbols involved.

### Step 4 — Identify Shared Foundations

Foundations are modules that multiple regions depend on and should be planned first:

- Database models and migrations
- Configuration and environment loading
- Authentication and authorization primitives
- Shared type definitions and interfaces
- Base classes and abstract interfaces

List each foundation with: module path, what depends on it, and why it's foundational.

### Step 5 — Surface Risk Hotspots

Risk hotspots are files or patterns that pose conflict or failure risk during parallel editing:

- **Multi-region hub files**: files referenced in 3+ explorer reports → high conflict risk during OCC
- **Complex coupling**: modules with 10+ imports from other regions → fragile to changes
- **Missing tests**: regions with no test coverage noted by explorers → regression risk
- **Stale or deprecated code**: areas flagged by explorers as having FIXME/TODO/deprecated markers

### Step 6 — Note Exploration Gaps

Gaps are areas the coordinator should create `expandable: true` tasks for:

- Explorer agents that failed or timed out
- `coverage_gaps` reported by successful explorers
- Large subdirectories that got only shallow coverage
- Regions outside the explored set that cross-references point to

---

## Output Schema

Return ONLY valid JSON:

```json
{
    "unified_structure": "<markdown: one paragraph per region summarizing purpose and architecture>",
    "cross_cutting_concerns": [
        "<pattern>: <specific files/symbols involved>"
    ],
    "shared_foundations": [
        "<module path>: <what depends on it and why it's foundational>"
    ],
    "risk_hotspots": [
        "<file or pattern>: <why it's risky>"
    ],
    "exploration_gaps": [
        "<area>: <why it's a gap and what should be done>"
    ],
    "total_files_discovered": <int>,
    "explorer_count": <int>,
    "total_duration_ms": <int>
}
```

---

## Quality Bar

A good synthesis enables a coordinator to:
1. **Identify foundation tasks** from `shared_foundations` (these have no dependencies)
2. **Set `touches_paths` accurately** from `unified_structure` and `risk_hotspots`
3. **Create expandable tasks** for `exploration_gaps`
4. **Set `depends_on` correctly** from `cross_cutting_concerns`
5. **Prioritize high-risk work** from `risk_hotspots`

If your synthesis doesn't help with all 5, it's incomplete.
