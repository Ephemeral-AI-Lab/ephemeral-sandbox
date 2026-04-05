---
name: codebase-analysis
description: Deep codebase analysis skill for coordinators. Guides systematic file reading and code understanding before task decomposition, producing a structured codebase map that informs precise task planning.
---

# Codebase Analysis

You are a coordinator that needs deep understanding of the codebase before decomposing work. Unlike a shallow directory or symbol survey, this skill guides you to **read actual source code** to understand implementation details, control flow, and logic — producing a structured analysis that drives precise task decomposition.

This skill is **depth-agnostic** — use it for root-level planning or mid-tree expansion when the code is unfamiliar or the task requires understanding existing behavior.

---

## When to Use This Skill

- **Bug fixes / regressions**: You need to understand the current behavior before planning a fix
- **Refactoring**: You need to map existing code structure and dependencies
- **Feature additions**: You need to understand integration points
- **Evolution tasks**: You need to understand what changed between versions

**Do NOT use for**:
- Greenfield tasks where no existing code needs understanding
- Simple tasks where a quick structural or symbol survey already gives enough context

---

## The Analysis Process

### Phase 1 — Structural Survey (broad, shallow)

Start with the repo-analysis tools actually available in this run to build a high-level map:

1. Perform one **shallow repo structure survey**. Identify:
   - Core source directories (where the main logic lives)
   - Test directories
   - Configuration files (setup.py, pyproject.toml, etc.)
   - Entry points (main.py, app.py, __init__.py)

2. If symbol or semantic navigation tools are available, inspect the major directories for key classes and functions. Otherwise use targeted search and nearby entry-point file reads to build the same mental model.

3. If hotspot or change-coupling hints are available, note the likely integration points.

### Phase 2 — Targeted Deep Reading (narrow, deep)

Based on the structural survey, read files that are **directly relevant** to the task:

1. **Entry points first**: Read the main module file(s) where the task's behavior originates.
2. **Follow the call chain**: When you find the key function/class, read the files it imports from or delegates to.
3. **Read relevant tests**: Tests reveal expected behavior and edge cases.
4. **Read configuration**: Setup files, manifests, and configs reveal constraints.

### Phase 3 — Build the Analysis Map

After reading, synthesize your understanding into a structured analysis:

```
## Codebase Analysis

### Architecture
- <module> is organized as: [description]
- Entry point: <file>:<function> handles [what]
- Key classes: <Class1> (in <file1>), <Class2> (in <file2>)

### Relevant Code Paths
- [Task-relevant behavior] flows through: <file1>:<func1> → <file2>:<func2> → ...
- Current implementation: [description of what the code does now]
- Key condition/logic at <file>:L<line>: [what it does and why it matters]

### Dependencies & Coupling
- <file1> imports from <file2> for [purpose]
- Shared state: [description of any shared state or globals]
- Hot files: [files that multiple concerns touch]

### Test Coverage
- Relevant tests in <test_file>: [what they test]
- Edge cases covered: [list]
- Gaps: [what's not tested]

### Impact Assessment
- Files that MUST change: [list with reasons]
- Files that MIGHT change: [list with conditions]
- Files that must NOT change: [list — regression risk]
```

---

## Reading Strategy by Task Type

### Bug Fix / Regression

1. Read the file where the bug manifests (from the bug report or changelog)
2. Read the specific function/method identified in the report
3. Read the git-relevant context (the function's callers and callees)
4. Read existing tests for the affected function
5. Read any test patch to understand what the expected behavior should be

**Goal**: Understand the exact code path, the current (broken) behavior, and the expected (correct) behavior.

### Feature Addition

1. Read the module where the feature will be added
2. Read adjacent modules that the feature will integrate with
3. Read tests to understand the testing patterns used
4. Read configuration to understand feature flags or settings

**Goal**: Understand the integration surface and existing patterns to follow.

### Refactoring

1. Read ALL files in the module being refactored
2. If semantic reference tracing is available, use it to find callers of symbols being moved/renamed. Otherwise use targeted search and imports to map the callers.
3. Read each caller to understand how the symbol is used
4. Read tests to ensure refactoring preserves behavior

**Goal**: Map the full blast radius before planning parallel work.

---

## Budget Guidelines

To avoid overwhelming the LLM context:

- **Small task** (1-3 files affected): Read up to 5 files, ~200 lines each
- **Medium task** (4-10 files): Read up to 8 files, focus on key functions (use start_line/max_lines)
- **Large task** (10+ files): Read key files fully, skim others with targeted line ranges

Prefer targeted line-range reads over full-file reads when a symbol map, grep hit, or structural survey already isolated the relevant section.

---

## Integration with Task Decomposition

After completing the analysis, use your findings to:

1. **Set accurate `touches_paths`** on each task based on files you read
2. **Set `touches_symbols`** based on the specific functions/classes affected
3. **Write detailed task descriptions** that reference specific code:
   - "Modify `prepare_url()` in `requests/models.py:349` to change the scheme check from `startswith(('http://', 'https://'))` to handle `http+unix://` URLs"
   - NOT "Fix the URL handling code"
4. **Identify real dependencies** based on call chains you traced
5. **Flag hot files** for OCC awareness based on coupling you discovered

---

## Self-Check Before Planning

1. Did I read the actual source files relevant to the task (not just symbols)?
2. Do I understand the current behavior of the code I'm planning to change?
3. Can I name the specific functions/lines that need modification?
4. Have I read the relevant tests to understand expected behavior?
5. Does my task graph reference specific files and code locations?
6. Are my `touches_paths` and `touches_symbols` based on actual code reading?

---

## Output Contract

After analysis, proceed to task decomposition using your analysis map. Every task description should reference specific files, functions, and line numbers discovered during analysis. Generic descriptions like "fix the bug" or "update the module" are insufficient — use what you learned from reading the code.
