---
name: deep-codebase-planning
description: Deep codebase planning protocol for coordinators. Guides a 4-phase planning process that bootstraps on generic skills, surveys code structure with the available repo-analysis tools, reads actual source files, and produces precise task graphs with specific file/function/line references.
---

# Deep Codebase Planning

You are a coordinator that must deeply understand the codebase before planning. This skill defines a strict 4-phase protocol that produces task graphs with precise code references — not vague descriptions.

---

## Phase 0 — Bootstrap

Before any analysis, bootstrap your context:

1. **`list_available_agents()`** — Discover available specialist agents. Use only exact names returned.
2. **Read your other skills first** — Start with the generic planning skills that shape decomposition (for example `changelog-decompose` and `codebase-analysis`) before benchmark-specific notes.
3. **Inspect the tooling actually available in this run** — Use the strongest repo-analysis capabilities exposed here, but do not assume any specific CI-query helper exists. Structural explorers, targeted file readers, grep/search, and semantic navigation are all acceptable when they are actually present.

Never skip the bootstrap order. Do not start with repo-wide file reads before skill guidance and a shallow structural survey tell you where to drill down.

### Choosing Discovery Tools

| Available capability | Strategy |
|-----------|----------|
| Structural survey + semantic navigation | Use structural survey to narrow the search, semantic navigation to confirm blast radius, and concrete file reads for implementation details |
| Structural survey + file readers/search | Map directories first, then read only the concrete production and test files relevant to the task |
| File readers/search only | Start from FAIL_TO_PASS roots, changelog keywords, and nearby files; keep lanes conservative and mark uncertain slices expandable |

---

## Phase 1 — Structural Survey (broad, shallow)

Build a high-level map of the codebase:

1. Perform one **shallow structural survey** with the broadest repo-analysis tool available in this run. Identify:
   - Core source directories (where main logic lives)
   - Test directories
   - Configuration files (setup.py, pyproject.toml, etc.)
   - Entry points (main.py, app.py, `__init__.py`)

2. If directory or symbol-level navigation is available, inspect only the directories relevant to the task and note key classes/functions. Otherwise use targeted search terms and entry-point file reads to build the same map.

3. If hotspot or change-coupling hints are available, note the likely integration points where parallel tasks may conflict.

**Output of Phase 1**: A mental map of which modules exist, which are relevant to the task, and where to drill deeper.

---

## Phase 2 — Deep Code Reading (narrow, deep)

Read the actual source files relevant to the task. This is what distinguishes deep planning from shallow directory-only planning.

### Planning Budget

For small or medium release-evolution tasks, keep the planning pass tight:

- inspect at most **2-3 narrow directories or symbol clusters**
- read at most **4-6 concrete production/test files**
- stop once you can name the owned production symbols, fixture surfaces, and verification boundary

Do **not** keep broadening the survey after you have a concrete task graph in mind. The goal is to plan, not to exhaustively reverse-engineer the whole repo.

### Reading Strategy

For each changelog item or task requirement:

1. **Read the primary file** where the behavior lives.
2. **Read the specific function/class region** once Phase 1 tells you where the relevant logic sits.
3. **Follow the call chain** by opening the imported or delegated files that actually participate in the behavior.
4. **Read relevant tests** because they reveal expected behavior, edge cases, and regression boundaries.
5. **Read configuration** when setup files or manifests constrain the behavior.

Prefer narrow path prefixes discovered from FAIL_TO_PASS roots. Avoid repo-wide symbol or file sweeps once you know the relevant leaf directories.

### Reading Budget

Stay within these limits to avoid context overflow:

| Task Size | Files | Lines per File |
|-----------|-------|---------------|
| Small (1-3 files affected) | Up to 5 files | ~200 lines each |
| Medium (4-10 files) | Up to 8 files | Targeted sections (start_line + max_lines) |
| Large (10+ files) | Key files fully, others targeted | Use the strongest available symbol/search/navigation capability to find relevant sections first |

### What to Look For

While reading, extract:
- **The exact function/method** that implements the behavior being changed
- **The condition or logic** that needs modification (note the line number)
- **Import chains** — what other files does this code depend on?
- **Error handling** — what happens on failure paths?
- **Constants/configuration** — are there hardcoded values or config-driven behavior?
- **Test patterns** — how are similar features tested?

---

## Phase 3 — Analysis Map

Synthesize your reading into a structured analysis. This map drives task decomposition.

### Template

```
## Analysis Map

### Architecture Overview
- Module layout: [what you learned from Phase 1]
- Entry point: <file>:<function> handles [what]
- Key classes: <Class1> (<file1>:L<line>), <Class2> (<file2>:L<line>)

### Code Paths (per changelog item)
For each changelog item:
- Current behavior: [what the code does now, with file:line references]
- Required change: [what needs to change]
- Affected function: <file>:<function> at line <N>
- Key condition: "<code snippet>" at <file>:L<line>

### Dependencies
- <file1>:L<line> imports <symbol> from <file2>
- Shared state: [globals, singletons, config objects]
- Hot files: [files touched by multiple changes]

### Test Coverage
- Relevant tests: <test_file>::<TestClass>::<test_method>
- Edge cases: [what's tested]
- Gaps: [what's not tested]

### Change Plan
- MUST change: [files with specific reasons]
- MIGHT change: [files with conditions]
- MUST NOT change: [files that would cause regressions]
```

---

## Phase 4 — Task Decomposition

Using your analysis map, decompose into a task graph:

### Task Description Requirements

Every task description MUST include:
1. **Specific changelog items** it covers (copy verbatim)
2. **Exact files to modify** with the functions/lines to change
3. **The current code** and what it should become (from your reading)
4. **Dependencies context** — what upstream tasks produce

When you cite code locations, use the files and line ranges you actually read. Do not invent approximate paths or line numbers from changelog wording alone.

**Good example:**
```
Modify PreparedRequest.prepare_url() in requests/models.py:349.

Current code at line 352:
  if ':' in url and not url.lower().startswith(('http://', 'https://')):

This condition incorrectly skips URL preparation for http+unix:// URLs.
Change the check to: not url.lower().startswith('http')
This allows http+unix://, httpx://, and http2:// URLs through preparation
while still skipping mailto:, data:, etc.

Also verify the url_parse() call at line 365 handles the resulting URL correctly.
```

**Bad example:**
```
Fix URL scheme handling in the requests library.
```

### Task Graph Shape

- **3-6 parallel module-scoped tasks** (expandable if >3 items per module)
- **0-2 sequential cross-cutting bridge tasks** (renames, API changes)
- **1 verification task** (depends on all, runs test suite)

### Setting Task Metadata

Based on your code reading:
- **`touches_paths`**: List exact files each task will modify
- **`touches_symbols`**: List functions/classes each task will touch
- **`depends_on`**: Only when task B needs task A's output (real data dependency, not "might edit same file")

---

## Self-Check Before Calling plan_tasks()

1. Did I bootstrap on the generic skills and adapt my strategy to the repo-analysis tools actually available?
2. Did I read the actual source files (not just symbols)?
3. Can I name the specific function and line number for each change?
4. Have I read the tests to understand expected behavior?
5. Does every task description reference specific code locations?
6. Are `touches_paths` based on files I actually read?
7. Is there a verification task at the end?

---

## Output Contract

Call `plan_tasks()` exactly once with the full task graph.
- Do not wait to be re-invoked.
- Never assign tasks to yourself or any coordinator.
- Never ask clarifying questions.
