# ExplorationLedger Architecture Diagram

## System Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        COORDINATION RUN LIFECYCLE                          │
│                                                                            │
│  ┌──────────┐    ┌───────────┐    ┌──────────────┐    ┌─────────────────┐  │
│  │ Executor │───▶│  Planning  │───▶│  Execution   │───▶│  Finalization   │  │
│  │  init    │    │  Workflow  │    │  (Workers)   │    │  & Export       │  │
│  └────┬─────┘    └─────┬─────┘    └──────┬───────┘    └────────┬────────┘  │
│       │                │                 │                     │           │
│       ▼                ▼                 ▼                     ▼           │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    EXPLORATION LEDGER (singleton)                   │   │
│  │                                                                     │   │
│  │  _files: dict[path → FileEntry]                                    │   │
│  │  _claims: dict[run_id → list[ExplorationClaim]]                    │   │
│  │  _run_parents: dict[child_run_id → parent_run_id]                  │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Data Flow — Writers & Readers

```
                          ┌─────────────────┐
                          │  ExplorationLedger│
                          │                   │
                          │  FileEntry:       │
                          │   exists          │
                          │   explored_by     │
                          │   exploration_depth│
                          │   symbols_exported│
                          │   claimed_by      │
                          │   modified_by     │
                          └─────────┬─────────┘
                                    │
              ┌─────────────────────┼─────────────────────┐
              │                     │                     │
         WRITERS                 READERS              LIFECYCLE
              │                     │                     │
    ┌─────────┴──────────┐  ┌──────┴───────────┐   ┌─────┴──────┐
    │                    │  │                  │   │            │
    ▼                    ▼  ▼                  ▼   ▼            ▼
┌────────┐ ┌──────────┐ ┌──────────┐ ┌─────────┐ ┌──────┐ ┌───────┐
│Explorer│ │ Dispatch │ │Validation│ │ Export  │ │Seed  │ │Cleanup│
│ Hooks  │ │          │ │(submit_  │ │         │ │from  │ │on     │
│        │ │          │ │ plan)    │ │         │ │plan  │ │finalize│
└───┬────┘ └────┬─────┘ └────┬─────┘ └────┬────┘ └──┬───┘ └───┬───┘
    │           │            │            │         │         │
    │record_    │record_     │batch_      │files_   │seed_    │clear_
    │exploration│file_claim  │path_exists │modified_│from_    │run
    │record_    │            │has_explor_ │by_task  │plan     │
    │file_      │            │ation_      │         │         │
    │explored   │            │covering    │         │         │
    │record_    │            │            │         │         │
    │symbols    │            │            │         │         │
    │           │            │            │         │         │
    ▼           ▼            ▼            ▼         ▼         ▼
 phase_     dispatch.    submit_      export.   executor. executor.
 hooks.py   py           plan.py      py        py        py
```

## Hierarchical Exploration Flow

```
LEVEL 0 — ROOT RUN
═══════════════════════════════════════════════════════════════

  analyze → regions: [src/services/, src/toolkits/, src/api/]
                │
                ▼
  explore → 3 explorer workers (scout-style, shallow)
                │
                ├─ record_exploration(run_id="root", scope="src/services/", depth=2)
                ├─ record_file_explored("src/services/resume.py", depth=2)
                ├─ record_symbols("resume.py", ["resume_coordination_run"])
                ├─ record_file_explored("src/services/export.py", depth=1)
                └─ record_file_explored("src/services/checkpoints.py", depth=3)
                        │
                        ▼
  synthesize → prose codebase_map (for LLM reasoning only, NOT parsed by code)
                        │
                        ▼
  plan_tasks → submit_plan with validation:
                ├─ batch stat() via _SandboxProxy  ── 1 shell exec for all paths
                └─ ledger.has_exploration_covering() ── scope match


LEVEL 1 — CHILD RUN (from expandable task "fix-resume")
═══════════════════════════════════════════════════════════════

  expansion/context.py:
    ├─ ledger.inherit_from_parent("root:fix-resume", "root")
    └─ ledger.get_parent_explored_summary("root", "src/services/coordination/")
                │
                ▼
  Child scoped context injected into project_context:
  ┌─────────────────────────────────────────────────────────┐
  │ ## Scoped Expansion                                     │
  │ - parent_explored_files: 3 files in this region         │
  │ - parent_exploration_depth: 3                           │
  │ - directive: GO DEEPER, do NOT re-explore               │
  │ - parent_file_details:                                  │
  │   - resume.py (depth: symbol-parsed,                    │
  │     symbols: resume_coordination_run, resolve_target)   │
  │   - export.py (depth: listed)          ◄── go read this │
  │   - checkpoints.py (depth: symbol-parsed,               │
  │     symbols: record_execution_attempt)                  │
  └─────────────────────────────────────────────────────────┘
                │
                ▼
  Child explore → workers go DEEPER (read files parent only listed,
                  trace call chains parent only read)
                │
                ├─ record_file_explored("export.py", depth=3)    ◄── deeper now
                ├─ record_file_explored("_helpers.py", depth=2)  ◄── new discovery
                └─ record_symbols("_helpers.py", ["build_context"])
                        │
                        ▼
  Child plan_tasks → validation walks parent chain:
                ├─ has_exploration_covering("_helpers.py")
                │   └─ not in _files.explored_by → check _claims
                │     └─ root claimed scope="src/services/" → prefix match → True ✓
                └─ batch stat() confirms file exists ✓


LEVEL 2 — GRANDCHILD RUN
═══════════════════════════════════════════════════════════════

  inherit_from_parent("root:fix-resume:subtask-1", "root:fix-resume")
  get_ancestor_findings walks: grandchild → child → root
                │
                ▼
  Sees ALL exploration from both ancestors
  Explores even deeper into specific logic paths
```

## Worker Runtime — Live Queries

```
┌──────────────────────────────────────────────────────────┐
│                    WORKER EXECUTION                      │
│                                                          │
│  1. Dispatch                                             │
│     └─ ledger.record_file_claim(path, task_id)           │
│                                                          │
│  2. Worker runs...                                       │
│     └─ calls query_exploration_context("resume.py")      │
│        ┌─────────────────────────────────────────┐       │
│        │ {                                       │       │
│        │   "path": "src/services/resume.py",     │       │
│        │   "in_ledger": true,                    │       │
│        │   "exists": true,                       │       │
│        │   "exploration_depth_label": "symbol-   │       │
│        │     parsed",                            │       │
│        │   "symbols": ["resume_coordination_run",│       │
│        │     "resolve_resume_target"],            │       │
│        │   "claimed_by": ["task-1", "task-2"],   │       │
│        │   "modified_by": "task-1",              │       │
│        │   "shared": true    ◄── be careful!     │       │
│        │ }                                       │       │
│        └─────────────────────────────────────────┘       │
│                                                          │
│  3. Completion                                           │
│     └─ ledger.record_file_mutation(path, task_id)        │
│                                                          │
│  4. Export                                               │
│     └─ ledger.files_modified_by_task(task_id)            │
│        └─ includes unplanned edits (not in touches_paths)│
└──────────────────────────────────────────────────────────┘
```

## Validation — Graceful Degradation

```
                    ┌─────────────┐
                    │ Plan Submit │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │ sandbox     │
                    │ available?  │
                    └──┬──────┬───┘
                   yes │      │ no
                       ▼      │
              ┌────────────┐  │
              │batch stat()│  │
              │ all paths  │  │
              └──┬─────┬───┘  │
            exist│     │miss  │
                 │     ▼      │
                 │  REJECT ◄──┘── only hard gate
                 │  "path does    (never bypassed)
                 │   not exist"
                 │
                 ▼
          ┌──────────────┐
          │   ledger      │
          │  available?   │
          └──┬────────┬───┘
          yes│        │no
             ▼        │
    ┌──────────────┐  │
    │has_exploration│  │
    │_covering?    │  │
    └──┬───────┬───┘  │
    yes│       │no    │
       │       ▼      │
       │    WARN ◄────┘
       │    (log only,
       │     don't reject)
       │
       ▼
    ALLOW ✓


  ┌─────────────────────────────────────────────┐
  │         DEGRADATION TABLE                   │
  ├─────────────────┬───────────┬───────────────┤
  │ Condition       │ sandbox   │ Decision      │
  ├─────────────────┼───────────┼───────────────┤
  │ exists+explored │ stat pass │ Allow ✓       │
  │ exists+!explored│ stat pass │ Allow + warn  │
  │ !exists         │ stat fail │ REJECT ✗      │
  │ no sandbox      │ —         │ ledger only   │
  │ no sandbox+     │ —         │ skip (allow)  │
  │  no ledger      │           │               │
  └─────────────────┴───────────┴───────────────┘
```

## Before vs After

```
BEFORE (prose-based)                    AFTER (live ledger)
════════════════════                    ═══════════════════

LLM generates prose ──┐                Explorer writes ──────┐
                      │                structured data       │
                      ▼                                      ▼
"pydantic/networks.py —          ┌──────────────────────────────┐
 FTP/WebSocket URL               │ FileEntry("networks.py")     │
 behavior is the                 │   explored_by: ["root"]      │
 dominant hotspot."              │   depth: 3                   │
                      │          │   symbols: ["UrlConstraints"]│
                      ▼          │   claimed_by: ["task-1"]     │
regex substring match            │   modified_by: null          │
on prose text         │          └──────────────┬───────────────┘
_CODEBASE_PATH_RE     │                         │
_CAMEL_SYMBOL_RE      │                         ▼
_PROVISIONAL_MARKERS  │              stat("networks.py") → exists?
                      │              has_exploration_covering? → yes
                      ▼
pass/fail depends on             Result: deterministic,
how LLM phrased its             based on filesystem truth
output                           not LLM phrasing

240 lines of regex    ──▶        ~60 lines of stat() + ledger
fragile, format-dependent        robust, format-independent

No child inheritance  ──▶        Full parent chain inheritance
Child = zero validation          Child sees all ancestor findings

Static snapshot       ──▶        Live updates from workers
Stale after 1st worker           Always current

Silent edit drops     ──▶        Export includes ledger mutations
at export time                   No more lost work
```
