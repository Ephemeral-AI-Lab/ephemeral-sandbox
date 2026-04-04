# Plan: ExplorationLedger — Live Code Intelligence for Coordination

## Problem Statement

The current planning workflow uses a static prose-based `codebase_map` produced once during the `synthesize` phase. This has three structural flaws:

1. **Static validation via regex on prose** — `submit_plan.py:147-389` parses LLM-generated natural language with substring matching and regex to validate plans. Whether validation passes depends on how the synthesis LLM phrased its output, not on actual codebase state.

2. **No context inheritance across expansion levels** — Child runs created from `expandable: true` tasks don't inherit the parent's codebase map or exploration findings. Child planners either re-explore from scratch (wasteful) or skip exploration (zero validation). The `expansion/context.py` passes down `project_context` text but not `phase_outputs`.

3. **No live awareness during execution** — Once workers start modifying files, the codebase map is stale. New files created by Worker A are invisible to Worker B's sub-planner. Edits to files outside `touches_paths` are silently dropped at export time.

## Design Overview

### Two-Layer Architecture

```
Layer 1: Prose Map (for LLM reasoning — UNCHANGED)
  - Synthesis still produces prose codebase_map
  - Fed to plan_tasks phase as reading context
  - Helps the planner LLM reason about architecture
  - NOT used for programmatic validation

Layer 2: Exploration Ledger (for code validation — NEW)
  - Structured data, not prose
  - Written by explorers, updated by workers
  - Inherited by child runs
  - Used by submit_plan validation (replaces regex grounding)
  - Used by export (catches unplanned edits)
```

### Core Data Model

```python
@dataclass
class FileEntry:
    exists: bool
    explored_by: list[str]        # run_ids that explored this file
    exploration_depth: int         # how deep exploration went (0=stat only, 1=listed, 2=read, 3=symbol-parsed)
    symbols_exported: list[str]    # from tree-sitter / LSP (populated lazily)
    claimed_by: list[str]          # task_ids that declared this in touches_paths
    modified_by: str | None        # task_id that last modified it
    category: str                  # "exclusive" | "shared" — auto-computed from claimed_by count

@dataclass  
class ExplorationClaim:
    run_id: str
    scope: str                     # glob pattern or directory path
    depth_reached: int
    findings_summary: dict         # structured exploration output
    parent_run_id: str | None      # for inheritance chain
```

## Implementation Steps

### Step 1: Create ExplorationLedger class

**File:** `backend/src/services/coordination/infrastructure/exploration_ledger.py` (new)

Core responsibilities:
- Thread-safe in-memory index (same pattern as `change_awareness.py`)
- File entry CRUD with atomic updates
- Exploration claim tracking per run
- Inheritance queries (what did ancestors explore?)

```python
class ExplorationLedger:
    def __init__(self):
        self._files: dict[str, FileEntry] = {}
        self._claims: dict[str, list[ExplorationClaim]] = {}  # run_id -> claims
        self._lock = threading.Lock()

    # --- Writers ---
    def record_exploration(self, run_id, scope, findings, parent_run_id=None): ...
    def record_file_stat(self, path, exists): ...
    def record_file_claim(self, path, task_id): ...
    def record_file_mutation(self, path, task_id): ...
    def record_symbols(self, path, symbols): ...

    # --- Readers ---
    def file_exists(self, path) -> bool | None: ...          # None = unknown
    def has_exploration_covering(self, path) -> bool: ...
    def get_ancestor_findings(self, run_id) -> list[ExplorationClaim]: ...
    def files_modified_since(self, timestamp) -> list[str]: ...
    def file_category(self, path) -> str: ...                 # auto from claimed_by count
    
    # --- Lifecycle ---
    def seed_from_plan(self, plan_tasks): ...                 # bulk-register touches_paths
    def inherit_from_parent(self, child_run_id, parent_run_id): ...
```

**Key design decisions:**
- In-memory singleton (like `change_awareness_service`), not DB-backed. Exploration data is ephemeral per coordination run tree.
- `file_exists` returns `None` for unknown files (not in index). Callers must `stat()` in sandbox to resolve unknowns. This avoids the cold-start problem — unknown != nonexistent.
- `inherit_from_parent` copies parent's file entries and claims into child's view. Child's own exploration adds/overwrites on top.

### Step 2: Wire explorers to write structured findings

**Files modified:**
- `backend/src/services/coordination/planning/workflow/phase_hooks.py` — add output hook for explore phase
- `backend/src/services/coordination/planning/workflow/phase_runner.py` — pass ledger to phase hooks

After the explore phase completes, the existing `_normalize_explore_output` hook at `phase_hooks.py:68` processes region reports. Add a secondary hook that writes structured data to the ledger:

```python
def _record_exploration_in_ledger(explore_output, run_id, ledger):
    for report in explore_output.get("region_reports", []):
        if report.get("status") not in ("completed", "partial_success"):
            continue
        content = _parse_report_content(report)
        scope = report.get("region", "")
        ledger.record_exploration(
            run_id=run_id,
            scope=scope,
            findings={
                "files": content.get("key_files", []),
                "symbols": content.get("key_symbols", []),
                "imports": content.get("import_graph", {}),
            },
        )
        # Record individual files discovered
        for file_path in content.get("files_read", []):
            ledger.record_file_stat(file_path, exists=True)
        for file_path in content.get("key_files", []):
            if isinstance(file_path, dict):
                ledger.record_file_stat(file_path["path"], exists=True)
                ledger.record_symbols(file_path["path"], file_path.get("symbols", []))
```

The prose codebase_map is still produced by the synthesize phase for the planner LLM to read. The ledger is a parallel structured channel.

### Step 3: Replace prose grounding with ledger + stat() validation

**Files modified:**
- `backend/src/posthooks/submit_plan.py` — replace `_validate_codebase_map_grounding` (240 lines deleted, ~60 lines added)

```python
def _validate_plan_grounding(
    tasks: dict[str, Any],
    ledger: ExplorationLedger | None,
    sandbox: Any | None,
) -> str | None:
    """Validate task paths against live state instead of prose."""
    if sandbox is None:
        return None  # can't validate without sandbox access
    
    for task in tasks.values():
        for path in getattr(task.ci_plan, "touches_paths", []) or []:
            normalized = _normalize_task_path(path)
            if not normalized:
                continue
            
            # Check 1: does file exist in sandbox?
            if not _sandbox_path_exists(sandbox, normalized):
                return (
                    f"Task '{task.task_id}' declares path '{normalized}' "
                    f"which does not exist in the workspace."
                )
            
            # Check 2: was this area explored? (warning, not rejection)
            if ledger and not ledger.has_exploration_covering(normalized):
                # Permissive: file exists but wasn't explored
                # Log warning but allow — stat() is the hard gate
                logger.warning(
                    "Task %s path %s exists but was not covered by exploration",
                    task.task_id, normalized,
                )
    
    return None
```

**What gets deleted from submit_plan.py:**
- `_get_synthesized_codebase_map` (lines 147-159)
- `_section_paths` (lines 162-167)
- `_collect_provisional_symbols` (lines 170-183) — regex NLP on prose
- `_path_is_grounded_in_codebase_map` (lines 262-269) — substring matching
- `_collect_entrypoint_symbols` (lines 280-296) — keyword matching
- `_validate_codebase_map_grounding` (lines 299-389) — the full prose validator
- `_collect_grounded_symbols_from_explore` (lines 216-259) — recursive dict walker

**Total: ~240 lines of regex/NLP deleted, ~60 lines of stat()+ledger queries added.**

### Step 4: Wire worker completions to update the ledger

**Files modified:**
- `backend/src/services/coordination/engine/worker_hooks.py` — add ledger update on worker completion
- `backend/src/services/coordination/engine/dispatch.py` — seed ledger with task claims on dispatch

On dispatch (`dispatch.py`):
```python
# In _dispatch_worker, after task is assigned:
if ctx.exploration_ledger:
    for path in task.ci_plan.touches_paths or []:
        ctx.exploration_ledger.record_file_claim(path, task.task_id)
```

On completion (`worker_hooks.py`):
```python
# In make_worker_hook, after artifact is built:
if ctx.exploration_ledger and artifact:
    for path in artifact.get("changed_files", []):
        ctx.exploration_ledger.record_file_mutation(path, task.task_id)
    for path in artifact.get("new_files", []):
        ctx.exploration_ledger.record_file_stat(path, exists=True)
        ctx.exploration_ledger.record_file_mutation(path, task.task_id)
```

### Step 5: Pass exploration context to child runs (hierarchical inheritance)

**Files modified:**
- `backend/src/services/coordination/engine/expansion/context.py` — add ledger inheritance
- `backend/src/services/coordination/engine/expansion/expansion.py` — pass ledger reference
- `backend/src/services/coordination/infrastructure/run_context.py` — add ledger to RunContext

Add `exploration_ledger` to `RunContext` dataclass:
```python
@dataclass
class RunContext:
    ...
    exploration_ledger: ExplorationLedger | None = None
```

Modify `build_scoped_project_context` in `expansion/context.py` to include parent exploration summary:
```python
def build_scoped_project_context(
    *,
    inherited_project_context: str,
    task_id: str,
    task_description: str,
    parent_run_id: str,
    parent_task_id: str,
    current_depth: int,
    max_depth: int,
    expansion_hint: str,
    exploration_ledger: ExplorationLedger | None = None,  # NEW
) -> str:
    # ... existing code ...
    
    # NEW: append parent exploration context for the child's scoped region
    if exploration_ledger:
        exploration_ledger.inherit_from_parent(
            child_run_id=f"{parent_run_id}:{task_id}",
            parent_run_id=parent_run_id,
        )
        parent_findings = exploration_ledger.get_ancestor_findings(parent_run_id)
        if parent_findings:
            explored_files = _collect_relevant_files(parent_findings, expansion_hint)
            scoped_section += "\n".join([
                f"- parent_explored_files: {len(explored_files)} files in this region",
                f"- parent_exploration_depth: {max(f.exploration_depth for f in explored_files) if explored_files else 0}",
                "- directive: GO DEEPER into the scoped region, do NOT re-explore files the parent already covered",
            ])
    
    return f"{base_context}\n\n{scoped_section}"
```

### Step 6: Update the explore skill for depth-aware exploration

**Files modified:**
- `.super-cocoa-agents/skills/coordination-synthesize/SKILL.md` — minor update
- New skill reference: `.super-cocoa-agents/skills/coordination-explore/references/depth-guidance.md`

Add to the explore skill's instructions:

```markdown
## Depth-Aware Exploration (Scoped Expansion)

When running inside a child expansion (detected by `## Scoped Expansion` in project context):

1. Read `parent_explored_files` from the scoped context — these files were already explored by the parent. Do NOT re-read them unless you need deeper symbol-level detail.
2. Read `parent_exploration_depth` — your exploration should go DEEPER than this level:
   - Parent depth 1 (file listing) → You should read files and extract key functions
   - Parent depth 2 (file reading) → You should trace call chains and dependencies
   - Parent depth 3 (symbol parsing) → You should analyze specific logic paths
3. Focus on the `parent_expansion_hint` region — do not explore broadly outside this scope.
4. Your findings AUGMENT the parent's, they don't replace them.
```

Update the synthesize skill to acknowledge the ledger:

```markdown
## Ledger Integration (added)

Your prose codebase_map is consumed by the planner LLM as reading context.
It is NOT used for programmatic validation — that is handled by the ExplorationLedger.
You do not need to worry about exact path substring coverage for downstream grounding.
Focus on producing a clear, useful architectural summary for the planner to reason with.
```

### Step 7: Connect ledger to export path (fix silent edit drops)

**Files modified:**
- `backend/src/services/coordination/export.py` — use ledger data alongside touches_paths

```python
def collect_completed_task_paths(plan, store, ledger=None):
    paths = set()
    for task in _completed_tasks(plan, store):
        # Existing: declared paths
        paths.update(task.ci_plan.touches_paths or [])
        # NEW: also include files the worker actually modified (from ledger)
        if ledger:
            modified = ledger.files_modified_by_task(task.task_id)
            paths.update(modified)
    return paths
```

This closes the gap where a worker edits a file not in its `touches_paths` — the edit is no longer silently dropped at export.

### Step 8: Add ledger to executor lifecycle

**Files modified:**
- `backend/src/services/coordination/engine/executor.py` — create ledger on run start, pass to RunContext

```python
class ExecutionEngine:
    def __init__(self, ...):
        ...
        self._exploration_ledger = ExplorationLedger()
    
    async def execute(self, plan, ...):
        # Seed ledger with plan's declared paths
        self._exploration_ledger.seed_from_plan(plan.tasks)
        
        ctx = RunContext(
            ...
            exploration_ledger=self._exploration_ledger,
        )
```

## Validation Strategy

### Graceful Degradation Table

| Condition | stat() | Ledger explored? | Decision |
|---|---|---|---|
| File exists + explored | pass | yes | Allow (full confidence) |
| File exists + NOT explored | pass | no | Allow + warn (stat is floor) |
| File does NOT exist | fail | — | Reject (hard gate) |
| Ledger unavailable | pass | unknown | Allow (fallback to stat-only) |
| Sandbox unavailable | unknown | — | Allow (no validation possible) |

### What Changes Per Scenario

| Scenario | Current behavior | New behavior |
|---|---|---|
| No codebase map | Zero validation | stat() still validates paths |
| Exploration failed | Zero validation | stat() still validates paths |
| Generic/vague map | Over-rejects real files | stat() allows real files |
| Child expansion | Zero validation (no inheritance) | Inherits parent findings + stat() |
| Worker edits unplanned file | Silently dropped at export | Captured by ledger, included in export |

## Files Created

| File | Lines (est.) | Purpose |
|---|---|---|
| `infrastructure/exploration_ledger.py` | ~200 | Core ledger class |

## Files Modified

| File | Change Summary | Lines Changed (est.) |
|---|---|---|
| `posthooks/submit_plan.py` | Delete prose grounding (240 lines), add stat+ledger validation (60 lines) | -180 net |
| `engine/worker_hooks.py` | Add ledger mutation recording | +15 |
| `engine/dispatch.py` | Add ledger claim recording | +10 |
| `engine/executor.py` | Create ledger, pass to RunContext | +10 |
| `engine/expansion/context.py` | Add exploration inheritance to scoped context | +30 |
| `engine/expansion/expansion.py` | Pass ledger reference | +5 |
| `infrastructure/run_context.py` | Add `exploration_ledger` field | +2 |
| `export.py` | Include ledger-tracked mutations in export paths | +15 |
| `planning/workflow/phase_hooks.py` | Write explore findings to ledger | +40 |
| `planning/workflow/phase_runner.py` | Pass ledger to hooks | +5 |
| `.super-cocoa-agents/skills/coordination-synthesize/SKILL.md` | Clarify prose map is for LLM only | +5 |
| `.super-cocoa-agents/skills/coordination-plan-tasks/SKILL.md` | Remove prose grounding references | ~10 changed |

## Risks and Mitigation

| Risk | Mitigation |
|---|---|
| Ledger is single point of mutable state | Same pattern as `change_awareness_service` singleton — proven thread-safe in production |
| Import graph maintenance is expensive | Don't maintain full import graph in ledger. Symbols are populated lazily only when explorer reads a file. No re-scanning on mutation. |
| Stale reads under concurrency | Validation is permissive (stat is the hard gate, ledger coverage is a soft signal). Stale ledger = warning, not rejection. |
| Cold start (no exploration) | stat() is the floor — works without any exploration. Strictly better than current "no map = zero validation". |
| `file_category` classification | Auto-computed from `len(claimed_by)`. No manual classification needed. Dynamic as new claims arrive. |
| Child run inherits stale parent data | Inheritance copies parent entries, child exploration overwrites. Child's own exploration is always authoritative for its scope. |

## Dependency Order

```
Step 1 (exploration_ledger.py)        — no dependencies
Step 2 (explorer writes to ledger)    — depends on Step 1
Step 3 (submit_plan validation)       — depends on Step 1
Step 4 (worker hooks write to ledger) — depends on Step 1
Step 5 (child run inheritance)        — depends on Steps 1, 2
Step 6 (skill updates)                — depends on Step 5
Step 7 (export integration)           — depends on Step 1, 4
Step 8 (executor lifecycle)           — depends on Steps 1, 4, 5
```

Steps 2, 3, 4 can be done in parallel after Step 1.
Steps 5, 7 can be done in parallel after their dependencies.
Step 6 (skill updates) and Step 8 are final integration steps.
