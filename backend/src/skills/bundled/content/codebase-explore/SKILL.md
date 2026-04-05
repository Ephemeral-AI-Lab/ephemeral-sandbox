---
name: codebase-explore
description: Guides explorer agents to map a codebase region using hierarchical, frontier-first exploration, producing structured ExplorationReport JSON with symbols, patterns, risks, and cross-references.
---

# Codebase Exploration

You are exploring a specific region of a codebase. Your output feeds into a synthesis phase that merges reports from parallel explorers into a unified codebase map for task planning.

---

## Scope Discipline

Exploration in this system is hierarchical. Your job is to create the smallest useful map for synthesis, not to exhaustively understand the entire assigned region.

Classify the assigned scope before choosing depth:

- **Broad or root-like region**: repository root, top-level package, or directory with many major children. Treat any top-level package or a directory with roughly 10+ immediate files/children as broad unless the prompt already narrowed it further. Produce a frontier map only.
- **Focused region**: single subsystem directory or a small cluster of closely related files. You may do targeted inspection.
- **File-level region**: inspect the file directly and summarize its role.

For a broad or root-like region:

- Prioritize top-level structure, ownership boundaries, entry points, and likely hotspot children.
- Inspect only enough files to validate the frontier map.
- Defer implementation-heavy or child-heavy areas to deeper follow-up runs.
- Record intentionally deferred areas in `coverage_gaps`.
- Keep the runtime budget tight: use only a handful of tool calls and prefer omission over exhaustive follow-up.
- Hard cap: broad/root-like regions should usually stop after 2-4 tool calls total.
- Never read more than two files in a broad/root-like pass.
- Do not call `query_symbols` on the whole directory or package path for a broad/root-like region. If symbol evidence is still needed, target one specific file only.
- Do not mix a broad implementation survey with test-by-test enumeration in the same run.

When in doubt, bias shallow and explicit rather than deep and incomplete.

---

## Exploration Process

If you need a reminder of the live tool contract for this run, load
`references/runtime-tool-surface.md`. Treat the runtime overlay as authoritative
over examples in this skill.

Meta-tool discipline:

- Load `references/runtime-tool-surface.md` at most once if the runtime contract is unclear.
- Do not call `get_skill_script` in this skill.
- Do not call `query_recent_changes`, `query_recent_changes_for_paths`, `query_git_history`, or other history-oriented helpers in this skill.
- Use `query_ci_status()` at most once per run, and only when a structure or symbol result suggests the index may be stale or incomplete.
- Once the tool surface is clear, stop loading more skill references and continue with repo exploration only.

### Phase 1 — Structure Survey

1. `query_workspace_structure(region_path, 3)` — get the directory tree
2. Note file counts per subdirectory to identify the most important follow-up candidates
3. For broad or root-like regions, stop at the frontier unless a quick validation read is needed
4. Use `query_symbols()` only when symbol evidence is still needed after the structure survey:
   - for broad or root-like regions, target at most one specific boundary file
   - for focused regions, target one file or one already-narrow child slice
   - do not use `query_symbols(region_path)` on a large directory or package
5. If the symbol result is very large, do not keep querying adjacent directories just to accumulate more symbols
6. If `query_workspace_structure()` returns empty for the assigned region, treat the path as hidden, absent from indexing, or otherwise weakly visible. Do not broaden to the repo root or sibling directories. At most one narrow validation read may confirm the condition; then report the region as inaccessible in `coverage_gaps`.

### Phase 2 — Selective Inspection

Choose depth based on scope.

For broad or root-like regions:

1. `read_sandbox_file()` on 1-2 validation files only — confirm entry points, boundaries, or central hubs (NOT full implementations)
2. `query_symbols()` on at most 1 specific hotspot file if needed to confirm decomposition
3. `query_symbol_references()` on only 1 boundary symbol when cross-region evidence is truly needed
4. Do NOT try to exhaustively cover every major child; leave depth to follow-up runs
5. Absolute hard stop: never exceed 4 total tool calls in a broad/root-like pass, including any meta-tool call you chose to make

For focused regions:

1. `read_sandbox_file()` on 1-3 representative files only — read signatures, class definitions, imports (NOT full implementations)
2. `query_symbol_references()` on 1-2 key symbols — find cross-region imports
3. `query_symbols()` on high-priority files directly connected to the goal, not on broad siblings
4. Stop after 4-6 tool calls even if more nearby files look interesting; capture the remainder in `coverage_gaps`
5. Absolute hard stop: never exceed 6 total tool calls in a focused pass, including any meta-tool call you chose to make

For file-level regions:

1. `read_sandbox_file()` on the file once
2. Optionally use either `query_symbols()` or `query_symbol_references()` once if it materially improves the report
3. Stop after that and assemble the report
4. Absolute hard stop: never exceed 3 total tool calls in a file-level pass, including any meta-tool call you chose to make

Prioritize:
- Files with names matching goal keywords
- Entry points (main.py, __init__.py, index.ts, app.py)
- Files with highest symbol counts (complexity indicators)
- Files imported by many others (hub files)

### Phase 3 — Report Assembly (final 20s)

Synthesize findings into the ExplorationReport JSON.

---

## Output Schema

Return ONLY valid JSON:

```json
{
    "region": "<path you explored>",
    "structure": "<markdown directory tree with file counts>",
    "key_symbols": [
        {"name": "ClassName", "kind": "class", "file": "path/to/file.py", "line": 42},
        {"name": "function_name", "kind": "function", "file": "path/to/file.py", "line": 100}
    ],
    "patterns": ["<pattern1: e.g. 'Repository pattern with SQLAlchemy'>", "..."],
    "cross_references": ["<import from outside this region: e.g. 'models.User imported by auth/views.py'>"],
    "risk_areas": ["<file or pattern that is fragile, complex, or heavily coupled>"],
    "coverage_gaps": ["<subdirectory or area you did not have time to explore>"]
}
```

Keep the JSON compact:
- `structure` should be a short frontier summary, not a full listing
- `key_symbols` should usually contain 5-12 items, not every symbol discovered
- `patterns`, `cross_references`, `risk_areas`, and `coverage_gaps` should usually stay within 3-5 items each

---

## Quality Guidelines

- **key_symbols**: Include the 5-12 most important symbols. Prefer public APIs, entry points, and base classes over internal helpers.
- **patterns**: Name the architectural patterns you observe (MVC, repository, event-driven, middleware chain, etc.)
- **cross_references**: Critical for synthesis — these reveal how regions connect. Note BOTH directions (this region imports X, this region is imported by Y).
- **risk_areas**: Files with high cyclomatic complexity, files that import from 5+ other modules, files with TODO/FIXME/HACK comments, test files with many skipped tests.
- **coverage_gaps**: Be honest about what you couldn't explore. Better to flag a gap than silently skip it. For broad or root-like regions, use this field to name major child areas intentionally deferred for deeper follow-up.
- For broad or root-like regions, prefer boundary symbols and hotspot directories over internal implementation detail.
- Favor shorter summaries over completeness when the tool outputs are already large.

---

## Time Management

You have ~90 seconds with a SUMMARIZE_NOW signal at ~75s.

- Broad or root-like regions should spend most of the budget on Phase 1 and only a small amount on validation reads.
- If you have already confirmed the frontier and used the scope's tool-call budget, stop immediately and switch to report assembly.
- If a broad or root-like region already has a stable frontier after 2-3 calls, stop there instead of spending the remaining budget.
- If you receive SUMMARIZE_NOW: immediately stop exploring and emit your best report from data collected so far
- Do NOT start new tool calls after the signal
- An incomplete report with honest coverage_gaps is better than no report
- If you already have enough information for a compact report, stop early rather than spending the remaining budget

---

## Anti-Patterns

- **Don't read full file contents** — read headers, signatures, and first ~50 lines of key files
- **Don't reread the same file in multiple windows** unless the first read clearly missed the required boundary
- **Don't explore outside your region** — note cross-references but don't follow them
- **Don't try to understand implementation details** — map structure and entry points
- **Don't recursively walk every top-level child of a broad or root-like region** — map the frontier and defer the rest
- **Don't turn a broad assignment into a full subsystem audit** — hierarchical exploration depends on shallow parent passes
- **Don't treat an empty structure result as permission to broaden scope** — keep the assigned path, record the gap, and stop
- **Don't run `query_symbols` on a whole package or directory just because it is the assigned region** — pick one boundary file or skip symbols entirely
- **Don't call `get_skill_script` or change-history helpers** — this skill is about structure, boundaries, and cross-region interfaces, not script loading or git archaeology
- **Don't re-check CI/index health repeatedly** — `query_ci_status()` is a one-time fallback, not a routine exploration step
- **Don't treat the tool-call budget as advisory** — broad/root-like passes cap at 4 total calls, focused passes at 6, file-level passes at 3
- **Don't emit empty arrays** — if you found nothing for a field, explain why in coverage_gaps
- **Don't keep retrying missing symbols or missing files** — after one miss, record the uncertainty and move on
