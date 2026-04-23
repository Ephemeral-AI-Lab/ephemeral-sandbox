# Code Intelligence

The Code Intelligence subsystem orchestrates multi-backend semantic and structural code querying across local and sandbox workspaces. It unifies LSP-driven queries (Jedi for Python, language servers for TypeScript) with fast symbol indexing, edit coordination, and file change tracking.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  Client                                                                      │
│  ┌──────────────────────────────────────────┐                               │
│  │  ci_query_symbol                         │                               │
│  │  ci_workspace_structure                  │                               │
│  │  ci_status                               │                               │
│  └──────────────────────┬───────────────────┘                               │
└─────────────────────────┼───────────────────────────────────────────────────┘
            query_symbol / find_definitions │
                                            ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  CodeIntelligenceService                                                     │
│                                                                              │
│  ┌──────────────────────────────┐    ┌───────────┐    ┌──────────────────┐  │
│  │  IntelligenceQueryRouter     │    │ SymbolIndex│    │   LspClient      │  │
│  │  (priority-based dispatch)   │    │ (background│    │  (Python: Jedi)  │  │
│  └────────────┬─────────────────┘    │   build)  │    └──────────────────┘  │
│       try LSP │        │ fallback    └─────┬──────┘                         │
│               │        │                   │ cache                           │
│               ▼        ▼             ┌─────▼──────┐                         │
│  ┌──────────────┐  ┌────────────┐    │  TreeCache │    ┌──────────────────┐  │
│  │LspBackend-   │  │SymbolIndex-│    │ (tree-     │    │   Arbiter        │  │
│  │Adapter       │  │Backend-    │    │  sitter)   │    │ (edit ledger)    │  │
│  │(priority:100)│  │Adapter     │    └────────────┘    └──────────────────┘  │
│  └──────┬───────┘  │(priority:  │                                            │
│         │          │  50)       │                      ┌──────────────────┐  │
│         │          └────────────┘                      │  TimeMachine     │  │
│         │                                              │ (undo snapshots) │  │
│         │                                              └──────────────────┘  │
│         │                                                                    │
│         │                                              ┌──────────────────┐  │
│         │                                              │  Patcher         │  │
│         │                                              │ (merge logic)    │  │
│         │                                              └──────────────────┘  │
└─────────┼──────────────────────────────────────────────────────────────────-┘
          │ goto_definition / find_references
          ▼
┌──────────────────────────────────────────────────┐
│  Backend Adapters                                 │
│                                                   │
│  ┌────────────────────────┐  ┌─────────────────┐  │
│  │  LspBackendAdapter     │  │ SymbolIndexBack- │  │
│  │  (priority: 100)       │  │ endAdapter       │  │
│  │  goto_definition       │  │ (priority: 50)   │  │
│  │  find_references       │  │ find / file_syms │  │
│  └──────────┬─────────────┘  └────────┬────────┘  │
└─────────────┼──────────────────────────┼───────────┘
              │                          │ read files
  subprocess / │                          ▼
  sandbox exec │          ┌──────────────────────────┐
              │          │  File Storage             │
              │          │  ┌──────────────────────┐ │
              ▼          │  │  Local Filesystem    │ │
┌─────────────────────┐  │  └──────────────────────┘ │
│  Local Filesystem   │  │  ┌──────────────────────┐ │
│  (jedi script)      │  │  │  Sandbox Filesystem  │ │
└─────────────────────┘  │  │  (daytona_sdk)       │ │
                         │  └──────────────────────┘ │
                         └──────────────────────────┘
```

## Components

### RoutingService (CodeIntelligenceService)

Singleton per sandbox. `CodeIntelligenceService` is intentionally a thin facade:
it wires per-sandbox collaborators together, owns initialization and sandbox
rebinding, and delegates domain behavior to focused modules. Keep new behavior in
the owner module below rather than growing `routing/service.py`.

**Responsibility Owners:**
- `routing/query_router.py`: priority-based semantic query dispatch
- `routing/rename_planner.py`: rename planning, dry-run preview, and rename fast paths
- `routing/mutation_service.py`: typed write/edit/delete/move and rename-plan commits
- `routing/command_executor.py`: `svc.cmd(...)` Git workspace audit execution
- `routing/scope_status.py`: live scope coordination packet generation
- `routing/telemetry.py`: status and telemetry response shaping

**Key Methods:**
- `find_definitions(file_path, symbol, line, character)` → `list[SymbolInfo]`
- `find_references(file_path, symbol, line, character)` → `list[ReferenceInfo]`
- `hover(file_path, line, character)` → `HoverResult | None`
- `diagnostics(file_path)` → `list[Diagnostic]`
- `query_symbols(query)` → `list[SymbolInfo]` (symbol index only)
- `apply_edit(request)` → `EditResult` (service-level edit helper)
- `cmd(sandbox, command, ...)` → process result with audited workspace mutations

**Initialization:**
- Symbol index builds asynchronously at first `ensure_initialized()` call
- LSP backends (Python/TypeScript) probed on demand; installed if missing

### Runtime Injection Contract

Daytona-backed tools use `sandbox.workspace.ensure_code_intelligence_runtime(...)`
as the shared injection boundary. The helper owns the runtime metadata contract:

- `daytona_sandbox`: the active Daytona sandbox handle
- `repo_root`: canonical repository root inside the sandbox
- `exec_cwd`: shell execution cwd, defaulting to `repo_root`
- `ci_workspace_root`: optional override for CI indexing root
- `ci_service`: the per-sandbox `CodeIntelligenceService`

`CodeIntelligenceService` exposes typed, OCC-gated mutation APIs that every
write-capable Daytona tool now calls directly. Edits / writes / renames flow
through `svc.write_file(specs)`, `svc.edit_file(specs)`, `svc.rename_symbol(...)`,
`svc.delete_file(paths)`, and `svc.move_file(specs)` — each call is one
`commit_operation_against_base` batch, so a single tool invocation is one OCC
boundary. Shell-style commands (daytona_shell, tests, builds) use `svc.cmd(sandbox,
command, ...)`, which runs the command inside a leased Git workspace slot and
commits the resulting Git diff through the same coordinator with
`strict_base=True`.
Both paths share the per-sandbox `Arbiter` ledger. Tools require `ci_service`
and pass typed specs; they do not carry concurrency state, base hashes,
transactions, diffs, edit labels, or audit path hints.

Callers may discover `repo_root` differently: sync toolkit prepare uses
`discover_workspace(...)`, async toolkit prepare uses
`discover_workspace_async(...)`, and lazy tool attach may resolve the sandbox on
first use. After discovery, all callers delegate metadata seeding and CI service
attachment to the shared helper. `daytona_cwd` remains a deprecated compatibility
alias for older tool contexts; new code should read `repo_root` and `exec_cwd`.

### Symbol Indexing (SymbolIndex)

Background daemon thread indexes Python files via AST, non-Python files via tree-sitter or regex fallback.

**Key Operations:**
- `ensure_built(wait=True, timeout=30.0)` → triggers background build
- `refresh(file_path, content)` → re-index single file after edit
- `find(query)` → search all indexed symbols by name
- `file_symbols(file_path)` → symbols in a specific file

**Data Flow:**
1. Collects indexable files from workspace root (local or sandbox)
2. For remote sandboxes: batch-downloads files via `sandbox.fs.download_files()` (fast) or individual fallback
3. Extracts symbols in parallel batches (SYMBOL_INDEX_BATCH_SIZE = 50)
4. Stores in thread-safe `_symbols: dict[str, _FileSymbols]`
5. Generation counter incremented on each operation commit

**Symbol Extraction:**
- **Python:** AST parser → walk recursively for functions, classes, assignments
- **Non-Python:** tree-sitter parse tree when available, else regex fallback patterns

### LSP Integration (LspClient)

Subprocess-based language server queries (Python via Jedi, TypeScript stub).

**Key Methods:**
- `goto_definition(file_path, line, character)` → `list[SymbolInfo]`
- `find_references(file_path, line, character)` → `list[ReferenceInfo]`
- `hover(file_path, line, character)` → `HoverResult | None`
- `diagnostics(file_path)` → `list[Diagnostic]`
- `ensure_ready(install_missing=False)` → `dict[str, bool]` (languages available)

**Python Backend (Jedi):**
- Runs Python script in subprocess (local) or sandbox (`sandbox.process.exec()`)
- Script calls `jedi.Script.goto()`, `get_references()`, `help()`
- Results parsed from JSON stdout
- Column resolution: advances from 0 to actual symbol name position on def/class lines

**Caching:**
- LRU cache with TTL (LSP_CACHE_TTL = 60 seconds, LSP_CACHE_MAX_ENTRIES = 200)
- Cache key: `def:{file_path}:{line}:{character}` etc.
- Invalidated on file edits via `invalidate(file_path)`

**Telemetry:**
- Tracks queries, cache hits, errors, successes per client instance

### Query Routing (IntelligenceQueryRouter)

Priority-based fallback dispatch across backends.

**Routing Logic:**
```
For each query (find_definitions, find_references, hover, diagnostics):
  1. Try backends in descending priority order
  2. Check if backend supports the file type
  3. Execute query; return if status == SUCCESS
  4. If status in {EMPTY, UNSUPPORTED, UNAVAILABLE, ERROR}, try next backend
  5. Return empty results if all backends fail
```

**Backend Priorities:**
- **LspBackendAdapter:** priority 100 (semantic queries preferred)
- **SymbolIndexBackendAdapter:** priority 50 (structural fallback)

### Audited Command Execution (`svc.cmd` + OverlayAuditor)

Shell-style commands run through `CodeIntelligenceService.cmd(...)`:

1. `svc.cmd(...)` builds a dangling Git snapshot of the live workspace. The
   snapshot captures tracked, dirty, and untracked non-ignored files while
   honoring `.gitignore`.
2. The user command runs in a fresh rootless overlay mount whose lowerdir is the
   live workspace and whose tmpfs upperdir captures command writes.
3. The sandbox-side overlay classifier walks upperdir, rejects disallowed
   `.git` and whiteout cases, direct-merges gitignored regular-file writes into
   the live workspace, and emits tracked changes as NDJSON.
4. The overlay committer builds one `OperationChange(strict_base=True)` per
   tracked change using `git show SNAP:path` as the base and submits the set as
   one `commit_operation_against_base` batch.
5. On success the coordinator records per-path ledger entries, invalidates LSP
   caches, refreshes the symbol index, and applies tracked changes to the live
   workspace. On `aborted_version`, tracked writes are skipped; gitignored
   direct-merges may already be live and are surfaced in overlay metadata.

daytona_shell, the test/build runners, and other shell-executing tools all go through
this one path. Repository diffs, transactions, and audit path hints no longer
live in the tool layer.

### File Content Management (ContentManager)

Abstraction layer for local and sandbox file I/O.

**Methods:**
- `read(file_path, allow_missing=False)` → `tuple[content, existed]`
- `write(file_path, content)` → writes via local FS or `sandbox.fs.upload_file()`
- `bind_sandbox(sandbox)` → updates sandbox handle for recycled services

### Tree Cache

Caches tree-sitter parse trees for reuse in symbol extraction and editing.

**Key Operations:**
- `get_tree(file_path, content=None)` → `TreeEntry | None`
- Limits: TREE_CACHE_MAX_FILES = 500, TREE_CACHE_MAX_FILE_SIZE = 1 MB

### Undo (TimeMachine)

Maintains file snapshots for undo.

**Methods:**
- `save(file_path, content)` → stores snapshot
- `rollback(file_path)` → `Snapshot | None`, restores previous version
- `clear()` → discards all snapshots

### Merge Logic (Patcher, merge.py)

Attempts to merge concurrent edits when file changes between prepare and commit.

**Conflict Detection:**
- If `current_hash != prepared.current_hash` and file existed at prepare time:
  1. Detect edit window (line_start, line_end) from diff
  2. Merge if edits don't overlap
  3. Return conflict if overlapping or edit window detection fails

## Symbol Indexing Workflow

```
  Tool (ci_query_symbol)        CodeIntelligenceService     IntelligenceQueryRouter
           │                              │                           │
           │── query_symbols(query) ─────▶│                           │
           │                              │── find(query) ──▶ SymbolIndex
           │                              │                           │
           │              ┌───────────────┤ [Index already built]     │
           │              │               │◀── list[SymbolInfo] ──────┤
           │              │               │                           │
           │              └───────────────┤ [Index building]          │
           │                              │◀── [] (empty) ────────────┤
           │◀─ fallback (ripgrep/regex) ──│                           │
           │                              │                           │
           │         [references=true]    │                           │
           │                              │── find_references() ─────▶│
           │                              │                           │── supports check
           │                              │          [Python/TypeScript file]
           │                              │                           │── find_references()
           │                              │                           │      │
           │                              │                       LspClient  │
           │                              │                           │◀─ run jedi subprocess
           │                              │◀── list[ReferenceInfo] ───│
           │◀─ definitions + references ──│                           │
           │                              │          [Unsupported]    │
           │                              │                           │── find_references()
           │                              │                           │      │
           │                              │                  SymbolIndexAdapter
           │                              │                           │◀─ UNSUPPORTED
           │◀─ [] ────────────────────────│◀─── [] ──────────────────-│
           │                              │                           │
           │         [references=false]   │                           │
           │◀─ definitions only ──────────│                           │
```

## LSP Query Sequence

```
  ci_query_symbol Tool    CodeIntelligenceService    LspClient (cache)    Python/Jedi
         │                          │                       │                   │
         │── find_definitions() ───▶│                       │                   │
         │                          │── _resolve_symbol_column()                │
         │                          │── find_definitions() via router            │
         │                          │── Check cache (key=def:file:line:char) ──▶│
         │                          │                       │                   │
         │             ┌────────────┤  [Cache hit]          │                   │
         │             │            │◀── cached results ────│                   │
         │             │            │                       │                   │
         │             └────────────┤  [Cache miss]         │                   │
         │                          │                       │── import jedi     │
         │                          │                       │   s=Script()      │
         │                          │                       │── s.goto(ln, col)▶│
         │                          │                       │◀─ JSON results ───│
         │                          │                       │── parse JSON      │
         │                          │                       │── store in LRU    │
         │                          │◀── results ───────────│   (TTL 60s)       │
         │◀─── definitions ─────────│                       │                   │
```

## Backend Adapter Protocol

```
┌──────────────────────────────────────────────────────────┐
│  <<interface>> CodeIntelligenceBackend                    │
│                                                           │
│  + name: str                                              │
│  + priority: int                                          │
│  + supports(file_path): bool                             │
│  + find_definitions(...): BackendQueryOutcome             │
│  + find_references(...): BackendQueryOutcome              │
│  + hover(...): BackendQueryOutcome                        │
│  + diagnostics(...): BackendQueryOutcome                  │
└───────────────────────┬──────────────────────────────────┘
                        │ implements
          ┌─────────────┴─────────────┐
          ▼                           ▼
┌──────────────────────┐   ┌───────────────────────────┐
│  LspBackendAdapter   │   │ SymbolIndexBackendAdapter  │
│                      │   │                            │
│  - _lsp: LspClient   │   │  - _index: SymbolIndex     │
│  + priority = 100    │   │  + priority = 50           │
│  + supports(...):    │   │  + supports(...):          │
│    .py .ts .js       │   │    True for all            │
│    .tsx .jsx         │   │                            │
└──────────┬───────────┘   └────────────┬───────────────┘
           │                            │
           ▼                            ▼
┌────────────────────────────────────────────────────────┐
│  BackendQueryOutcome                                    │
│                                                        │
│  + status: QueryStatus                                 │
│      (SUCCESS | EMPTY | UNSUPPORTED | UNAVAILABLE      │
│       | ERROR)                                         │
│  + results: list[Any]                                  │
│  + error: str                                          │
└────────────────────────────────────────────────────────┘
```

## Audited Mutation Workflow (single OCC boundary)

```
┌─────────────────────────────────────┐
│  Daytona mutation tool receives input│
└──────────────────┬──────────────────┘
                   │
                   ▼
┌─────────────────────────────────────┐    Typed batch APIs:
│  svc.{write,edit,delete,move}_file  │    one call = one batch
│  svc.rename_symbol(...)             │
│  svc.cmd(sandbox, command, ...)     │    Shell-style:
└──────────────────┬──────────────────┘    Git workspace diff -> batch
                   │
                   ▼
┌─────────────────────────────────────┐
│  Resolve OperationChange per slot   │
│  (base_hash, strict_base=True)      │
└──────────────────┬──────────────────┘
                   │
                   ▼
┌─────────────────────────────────────┐
│  commit_operation_against_base([…]) │
│  sorted locks, two-pass apply       │
└──────────────────┬──────────────────┘
                   │
         ┌─────────┴─────────┐
         ▼                   ▼
  ┌────────────┐      ┌──────────────┐
  │ committed  │      │ aborted_*    │
  │ → write +  │      │ → no writes, │
  │   record + │      │   clear abort│
  │   refresh  │      │   class to   │
  │   LSP      │      │   caller     │
  └────────────┘      └──────────────┘
```

## LSP Server Lifecycle

```
┌──────────────────────────────────┐
│  CodeIntelligenceService.__init__│
└──────────────────┬───────────────┘
                   │
                   ▼
┌──────────────────────────────────┐
│  LspClient(workspace_root,       │
│            sandbox)              │
└──────────────────┬───────────────┘
                   │
                   ▼
┌──────────────────────────────────┐
│  ensure_ready()                  │
│  check backends                  │
└──────────┬───────────────────────┘
           │
     ┌─────┴──────────────────┐
     │                        │
     ▼                        ▼
┌────────────────┐   ┌─────────────────────┐
│_check_python_  │   │_check_typescript_   │
│backend         │   │backend              │
│python3 -c      │   │npx tsc --version    │
│  import jedi   │   │                     │
└───────┬────────┘   └──────────┬──────────┘
        │                       │
        └──────────┬────────────┘
                   │
                   ▼
          ┌────────┴─────────┐
          │ Python available? │
          │ TypeScript avail? │
          └────────┬──────────┘
                   │
      ┌────────────┴─────────────┐
      │ local + missing          │ ready
      ▼                          ▼
┌────────────────┐      ┌───────────────────────────┐
│  Remote sandbox│      │  CodeIntelligenceService  │
│  + missing     │      │  (initialization done)    │
│  backends?     │      └───────────────────────────┘
└────────┬───────┘
    yes  │
    ┌────┴──────────────────────┐
    │                           │
    ▼                           ▼
┌──────────────────┐  ┌──────────────────────┐
│install_python_   │  │install_typescript_   │
│backend           │  │backend               │
│python3 -m pip    │  │npm install typescript│
│install jedi      │  │                    │
└────────┬─────────┘  └──────────┬───────────┘
         │                       │
         └──────────┬────────────┘
                    │ (retry ensure_ready)
                    ▼
         ┌──────────────────────┐
         │  ensure_ready()      │
         └──────────────────────┘
```

## Types and Data Structures

**SymbolInfo** – resolved symbol location
- `name: str` – full name (e.g., "MyClass.method")
- `kind: SymbolKind` – function, class, method, variable, module, interface, property, constant, unknown
- `file_path: str` – absolute file path
- `line: int` – 1-indexed line number
- `end_line: int | None` – optional end line for block symbols
- `character: int` – 0-indexed column
- `signature: str` – function/method signature snippet
- `docstring: str` – extracted documentation
- `container: str` – enclosing class/module name

**ReferenceInfo** – a reference to a symbol
- `file_path: str`
- `line: int` – 1-indexed
- `character: int` – 0-indexed
- `text: str` – matched line text

**HoverResult** – hover information at position
- `content: str` – docstring or signature
- `language: str` – source language
- `symbol: SymbolInfo | None` – resolved symbol info

**Diagnostic** – error/warning at position
- `file_path: str`
- `line: int`, `character: int`, `end_line`, `end_character`
- `severity: DiagnosticSeverity` – error, warning, information, hint
- `message: str`, `source: str`, `code: str`

**EditResult** – edit operation outcome
- `success: bool`
- `file_path: str`
- `message: str` – human-readable status
- `conflict: bool` – version conflict detected
- `conflict_reason: str` – version_mismatch, overlapping_range, stale_reservation, lock_timeout
- `snapshot_id: str` – arbiter generation on success

## Tool Surface (ci_query_symbol)

The unified tool for code intelligence queries.

```
ci_query_symbol(query, kind="", references=false) → ToolResult

Args:
  query (str): symbol name or partial name to search for
  kind (str): optional filter (function, class, method, variable)
  references (bool): if true, trace all callers/import sites via LSP

Returns (JSON):
  {
    "definitions": [
      {
        "name": "symbol_name",
        "kind": "function|class|method|variable|...",
        "file": "path/to/file.py",
        "line": 42,
        "signature": "def foo(x, y):"
      },
      ...
    ],
    "references": [  # only if references=true
      {
        "file": "path/to/caller.py",
        "line": 100,
        "text": "result = foo(a, b)"
      },
      ...
    ],
    "total_references": 25,  # only if references=true
    "confidence": "full|unavailable",  # "full" if LSP succeeded, else "unavailable"
    "reference_status": "lsp|definition_fallback",
    "lsp_reason": "python_backend_unavailable|no_lsp_references|..."  # fallback only
  }
```

**Routing Logic:**
1. Try `SymbolIndex.find(query)` first (fast, no position needed)
2. If empty, fall back to `ripgrep` regex search (local) or remote sandbox search
3. If `references=true`:
   - Ensure the Python LSP backend is ready, installing missing sandbox Jedi
     dependencies through `python3 -m pip` when possible
   - Try LSP `find_references()` on top 5 definitions (sorted by production priority)
   - If LSP unavailable or empty, return definitions only with
     `confidence: unavailable`, `reference_status: definition_fallback`, and an
     `lsp_reason`

**Common Patterns:**
- Find where a symbol is defined: `ci_query_symbol("MyClass", references=false)`
- Trace all callers before editing: `ci_query_symbol("my_function", references=true)`
- Narrow by kind: `ci_query_symbol("init", kind="method")`

## Fallback Strategies

**Symbol Index Cold Start:**
When the symbol index hasn't finished building, tools fall back to:
1. Local ripgrep (if available and workspace is local)
2. Remote ripgrep on sandbox via `sandbox.process.exec()`
3. Python regex fallback (no dependencies)

**LSP Fallback:**
When LSP is unavailable or fails on a query:
- `find_definitions`: fall back to symbol index
- `find_references`: no fallback (semantic-only); return empty
- `hover`: fall back to symbol index (line-based match)
- `diagnostics`: no fallback; return empty

**Multi-Workspace Support:**
- Each `CodeIntelligenceService` is per-sandbox singleton
- Service registry (`get_code_intelligence`) handles per-workspace instances
- Sandbox rebinding on service reuse: `_rebind_service_sandbox()`

## Telemetry (CITelemetry)

Runtime metrics aggregated from service components:
- `symbol_index_size: int` – total indexed symbols
- `symbol_index_generation: int` – index version counter
- `indexed_files: int` – files in symbol index
- `lsp_connected: bool` – at least one language backend ready
- `lsp_query_count: int` – total LSP queries
- `lsp_cache_hits: int` – cache hits in LspClient
- `arbiter_active_locks: int` – currently held Arbiter file locks
- `total_edits: int` – edits recorded in arbiter ledger

Accessible via `service.status()` → dict or `service.get_telemetry()` → CITelemetry.
