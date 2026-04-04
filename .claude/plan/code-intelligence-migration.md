# Implementation Plan: Migrate OCC/Code Intelligence System from synthetic-os

## Task Type
- [x] Backend (Python/FastAPI)

## Overview

Migrate the OCC (Optimistic Concurrency Control) / Code Intelligence system from synthetic-os into EphemeralOS as **three independent modules**:

1. **`services/sandbox`** — Sandbox lifecycle management (Daytona API wrapper)
2. **`services/code_intelligence`** — Core code intelligence runtime (AST, symbols, OCC, LSP)
3. **`tools/daytona_toolkit`** — Enhanced Daytona toolkit with CI integration (extends existing)

Each module is self-contained with clear interfaces between them.

---

## Technical Solution

### Architecture

```
┌──────────────────────────────────────────────────────┐
│                    Agent / Engine                      │
│                                                        │
│  ┌─────────────────┐   ┌───────────────────────────┐  │
│  │  DaytonaToolkit  │   │    CIToolkit (new)        │  │
│  │  (enhanced)      │   │    (read-only queries)    │  │
│  └────────┬─────────┘   └────────────┬──────────────┘  │
│           │                          │                  │
├───────────┼──────────────────────────┼──────────────────┤
│  Services │                          │                  │
│           ▼                          ▼                  │
│  ┌─────────────────┐   ┌───────────────────────────┐  │
│  │ SandboxService   │   │ CodeIntelligenceService   │  │
│  │ (lifecycle mgmt) │──▶│ (AST, symbols, OCC, LSP) │  │
│  └─────────────────┘   └───────────────────────────┘  │
│           │                          │                  │
│           ▼                          ▼                  │
│      Daytona SDK              tree-sitter / AST        │
└──────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **SandboxService** wraps Daytona SDK for lifecycle ops (create/start/stop/delete/list) and warmups CI on sandbox start. Independent of CI — can be used standalone.
2. **CodeIntelligenceService** is the core runtime: TreeCache, SymbolIndex, Arbiter (OCC), LSPClient, Ledger, TimeMachine. Exposed via a Gateway with stable public API.
3. **DaytonaToolkit** (existing) gets enhanced with CI-aware edit tools, LSP tools, and shell mutation detection via new mixins.
4. **CIToolkit** (new) provides read-only query tools for agents that need code grounding without write access.
5. Each module has its own `__init__.py` with clean public exports. Cross-module dependencies flow downward only.

---

## Implementation Steps

### Step 1 — Sandbox Service Module
**Deliverable:** `backend/src/ephemeralos/services/sandbox/`

Create the sandbox lifecycle service that wraps Daytona SDK operations.

| File | Operation | Description |
|------|-----------|-------------|
| `services/sandbox/__init__.py` | Create | Public exports: `SandboxService`, `SandboxInfo` |
| `services/sandbox/service.py` | Create | Core service: create/start/stop/delete/list/health/files/preview_url |
| `services/sandbox/types.py` | Create | Data models: `SandboxInfo`, `SandboxHealthResponse`, `CreateSandboxRequest`, `SandboxLabel` |

**Source reference:** `synthetic-os/backend/src/services/sandbox/sandbox_service.py` (664 lines)

**Adaptation notes:**
- Reuse existing `client.py` from `tools/daytona_toolkit/client.py` for Daytona SDK connection
- Remove `SPECIALIST_DEFINITIONS` / agent-assignment logic (EphemeralOS has its own agent system)
- Keep CI warmup as an optional hook (`on_sandbox_ready` callback) rather than hard dependency
- Keep git bootstrap logic (`ensure_git_available`)
- Labels use `ephemeralos` instead of `synthetic-os`

**Key functions to port:**
```python
class SandboxService:
    async def get_health() -> SandboxHealthResponse
    async def list_sandboxes() -> list[SandboxInfo]
    async def get_sandbox(sandbox_id: str) -> SandboxInfo
    async def create_sandbox(request: CreateSandboxRequest) -> SandboxInfo
    async def start_sandbox(sandbox_id: str) -> SandboxInfo
    async def stop_sandbox(sandbox_id: str) -> SandboxInfo
    async def delete_sandbox(sandbox_id: str) -> None
    async def list_files_recursive(sandbox_id: str, path: str, max_depth: int = 10) -> list[str]
    async def get_preview_url(sandbox_id: str, port: int) -> str
    async def list_snapshots() -> list[dict]
```

### Step 2 — Code Intelligence Service Module
**Deliverable:** `backend/src/ephemeralos/services/code_intelligence/`

Port the full OCC/CI runtime from synthetic-os.

| File | Operation | Description |
|------|-----------|-------------|
| `services/code_intelligence/__init__.py` | Create | Public exports: `CodeIntelligenceService`, `CodeIntelligenceGateway` |
| `services/code_intelligence/service.py` | Create | Main runtime: lifecycle, initialization, composite queries |
| `services/code_intelligence/gateway.py` | Create | Stable public interface with query/edit/cache mixins |
| `services/code_intelligence/tree_cache.py` | Create | AST caching with tree-sitter, per-file locking |
| `services/code_intelligence/symbol_index.py` | Create | Symbol lookup, background indexing, generational tracking |
| `services/code_intelligence/arbiter.py` | Create | OCC write coordination, per-file edit arbitration |
| `services/code_intelligence/lsp_client.py` | Create | LSP protocol integration, query caching, diagnostics |
| `services/code_intelligence/ledger.py` | Create | Edit audit journal, change awareness |
| `services/code_intelligence/time_machine.py` | Create | One-step undo snapshots, edit rollback |
| `services/code_intelligence/patcher.py` | Create | Validated edit patching with linting |
| `services/code_intelligence/query_router.py` | Create | Multi-backend query routing (LSP, SymbolIndex) with priority |
| `services/code_intelligence/backend_protocol.py` | Create | Protocol interface for CI backends |
| `services/code_intelligence/constants.py` | Create | Configuration constants |
| `services/code_intelligence/types.py` | Create | Shared data types (SymbolInfo, EditResult, QueryResult, etc.) |

**Source reference:** `synthetic-os/backend/src/services/code_intelligence/` (~11,718 lines across 30+ files)

**Adaptation notes:**
- Flatten the deeply nested directory structure from synthetic-os (runtime/, lsp/, editing/, analysis/) into a single-level module
- Replace agno SDK dependencies with EphemeralOS base classes
- Use EphemeralOS's existing `services/lsp/` module as foundation for the LSP client
- TreeCache: if `tree-sitter` is unavailable, fall back to Python `ast` module (already partially implemented in existing `services/lsp/__init__.py`)
- Lock ordering invariant must be preserved (Groups A-D from synthetic-os)
- Gateway pattern ensures service internals can change without breaking consumers

**Key interfaces:**
```python
class CodeIntelligenceGateway:
    # Query mixin
    async def find_definitions(file_path: str, symbol: str) -> list[SymbolInfo]
    async def find_references(file_path: str, symbol: str) -> list[ReferenceInfo]
    async def hover(file_path: str, line: int, character: int) -> HoverResult
    async def diagnostics(file_path: str) -> list[Diagnostic]
    async def query_symbols(query: str) -> list[SymbolInfo]
    
    # Edit mixin (OCC)
    async def apply_edit(file_path: str, edit: EditRequest) -> EditResult
    async def undo_last_edit(file_path: str) -> EditResult
    
    # Cache mixin
    async def prime_cache(file_paths: list[str]) -> None
    async def invalidate(file_path: str) -> None
    def get_telemetry() -> CITelemetry
```

### Step 3 — Enhance Existing Daytona Toolkit with CI Integration
**Deliverable:** Enhanced `backend/src/ephemeralos/tools/daytona_toolkit/`

Add CI-aware editing, LSP queries, and shell mutation detection to the existing toolkit.

| File | Operation | Description |
|------|-----------|-------------|
| `tools/daytona_toolkit/edit_tool.py` | Create | OCC-coordinated file editing via Arbiter |
| `tools/daytona_toolkit/lsp_tools.py` | Create | LSP hover, goto-definition, find-references, diagnostics |
| `tools/daytona_toolkit/ci_integration.py` | Create | Gateway acquisition, tree cache priming, agent ID resolution |
| `tools/daytona_toolkit/shell_mutation.py` | Create | Detect file changes from shell commands, update Ledger |
| `tools/daytona_toolkit/codeact_tool.py` | Create | Multi-step code thinking and execution tool |
| `tools/daytona_toolkit/toolkit.py` | Modify | Register new tools, add CI gateway injection |
| `tools/daytona_toolkit/__init__.py` | Modify | Export new tools |

**Source reference:** `synthetic-os/backend/src/toolkits/daytona_toolkit/` (19 files, ~8,157 lines)

**Adaptation notes:**
- New tools follow EphemeralOS `BaseTool` pattern (not agno mixins)
- CI integration is optional — tools degrade gracefully if no CI service is configured
- Edit tool coordinates with Arbiter for conflict detection, falls back to direct write if no CI
- Shell mutation detection hooks into `DaytonaBashTool` post-execution
- Keep tool ordering preference: read tools first, then write tools, then execution

**New tools to add:**
```python
# edit_tool.py
class DaytonaEditTool(BaseTool):
    name = "daytona_edit_file"
    # Coordinated edit via Arbiter with conflict detection
    # Snapshot management via TimeMachine

# lsp_tools.py  
class DaytonaLspHoverTool(BaseTool):
    name = "daytona_lsp_hover"

class DaytonaLspDefinitionTool(BaseTool):
    name = "daytona_lsp_definition"

class DaytonaLspReferencesTool(BaseTool):
    name = "daytona_lsp_references"

class DaytonaLspDiagnosticsTool(BaseTool):
    name = "daytona_lsp_diagnostics"

# codeact_tool.py
class DaytonaCodeActTool(BaseTool):
    name = "daytona_codeact"
    # Multi-step code thinking: Python, Node.js, Shell
```

### Step 4 — CI Toolkit (Read-Only Query Toolkit)
**Deliverable:** `backend/src/ephemeralos/tools/ci_toolkit/`

New lightweight toolkit for agents that need code grounding without write access.

| File | Operation | Description |
|------|-----------|-------------|
| `tools/ci_toolkit/__init__.py` | Create | Public exports: `CIToolkit` |
| `tools/ci_toolkit/query_tools.py` | Create | CI status, workspace structure, symbol queries |
| `tools/ci_toolkit/file_tools.py` | Create | Bounded file reads via CI cache |

**Source reference:** `synthetic-os/backend/src/toolkits/ci_toolkit/` (7 files)

**Tools:**
```python
class CIToolkit(BaseToolkit):
    name = "ci"
    tools = [
        CIStatusTool(),           # CI readiness summary
        WorkspaceStructureTool(), # Directory tree via CI cache
        SymbolQueryTool(),        # Symbol enumeration
        SymbolReferencesTool(),   # Cross-file references
        EditHotspotsTool(),       # Conflict-prone file detection
        RecentChangesTool(),      # Real-time change awareness
    ]
```

### Step 5 — API Routes
**Deliverable:** New FastAPI routers for sandbox and CI operations.

| File | Operation | Description |
|------|-----------|-------------|
| `server/routers/sandboxes.py` | Modify | Enhance with full SandboxService integration |
| `server/routers/code_intelligence.py` | Create | CI query/edit/stream endpoints |
| `server/app_factory.py` | Modify | Register CI router, initialize services in lifespan |

**Endpoints:**
```
# Sandbox (enhance existing)
GET    /api/sandboxes/health
GET    /api/sandboxes
POST   /api/sandboxes
GET    /api/sandboxes/{id}
POST   /api/sandboxes/{id}/start
POST   /api/sandboxes/{id}/stop
DELETE /api/sandboxes/{id}
GET    /api/sandboxes/{id}/files
GET    /api/sandboxes/{id}/preview-url
GET    /api/sandboxes/snapshots

# Code Intelligence (new)
GET    /api/code_intelligence/{sandbox_id}/status
GET    /api/code_intelligence/{sandbox_id}/query/definitions
GET    /api/code_intelligence/{sandbox_id}/query/references
GET    /api/code_intelligence/{sandbox_id}/query/hover
GET    /api/code_intelligence/{sandbox_id}/query/symbols
GET    /api/code_intelligence/{sandbox_id}/read/{filepath}
POST   /api/code_intelligence/{sandbox_id}/edit
POST   /api/code_intelligence/{sandbox_id}/undo
WS     /api/code_intelligence/{sandbox_id}/stream
```

### Step 6 — Factory Registration & Wiring
**Deliverable:** Register new toolkits in the factory system.

| File | Operation | Description |
|------|-----------|-------------|
| `tools/factory.py` | Modify | Add `ci` and `sandbox` toolkit factories |
| `tools/__init__.py` | Modify | Import and register CIToolkit |

**Factory additions:**
```python
def _create_ci(ctx: ToolkitContext) -> BaseToolkit:
    from ephemeralos.tools.ci_toolkit import CIToolkit
    sandbox_id = ctx.metadata.get("sandbox_id", "")
    return CIToolkit(sandbox_id=sandbox_id or None)

def _create_sandbox(ctx: ToolkitContext) -> BaseToolkit:
    # Sandbox management toolkit (if needed as tools)
    ...

register_toolkit_factory("ci", _create_ci)
```

### Step 7 — Database Persistence (Optional)
**Deliverable:** `backend/src/ephemeralos/db/stores/code_intelligence_store.py`

| File | Operation | Description |
|------|-----------|-------------|
| `db/models/code_intelligence.py` | Create | `SandboxEditJournalRecord`, `SandboxRuntimeSnapshotRecord` |
| `db/stores/code_intelligence_store.py` | Create | Ledger persistence, snapshot reconciliation |

**Adaptation notes:**
- Only needed if edit history persistence across restarts is required
- Can be deferred — CI service works in-memory without this

---

## Key Files (Summary)

| File | Operation | Description |
|------|-----------|-------------|
| `services/sandbox/__init__.py` | Create | Sandbox service module entry |
| `services/sandbox/service.py` | Create | Sandbox lifecycle management |
| `services/sandbox/types.py` | Create | Sandbox data models |
| `services/code_intelligence/__init__.py` | Create | CI module entry |
| `services/code_intelligence/service.py` | Create | Main CI runtime |
| `services/code_intelligence/gateway.py` | Create | Public gateway interface |
| `services/code_intelligence/tree_cache.py` | Create | AST caching |
| `services/code_intelligence/symbol_index.py` | Create | Symbol indexing |
| `services/code_intelligence/arbiter.py` | Create | OCC write coordination |
| `services/code_intelligence/lsp_client.py` | Create | LSP integration |
| `services/code_intelligence/ledger.py` | Create | Edit audit journal |
| `services/code_intelligence/time_machine.py` | Create | Undo snapshots |
| `services/code_intelligence/patcher.py` | Create | Edit patching |
| `services/code_intelligence/query_router.py` | Create | Multi-backend routing |
| `services/code_intelligence/types.py` | Create | Shared types |
| `tools/daytona_toolkit/edit_tool.py` | Create | OCC-aware edit tool |
| `tools/daytona_toolkit/lsp_tools.py` | Create | LSP query tools |
| `tools/daytona_toolkit/ci_integration.py` | Create | CI bridge for toolkit |
| `tools/daytona_toolkit/shell_mutation.py` | Create | Shell change detection |
| `tools/daytona_toolkit/codeact_tool.py` | Create | CodeAct tool |
| `tools/daytona_toolkit/toolkit.py` | Modify | Register new tools |
| `tools/ci_toolkit/__init__.py` | Create | CI toolkit entry |
| `tools/ci_toolkit/query_tools.py` | Create | Read-only CI query tools |
| `tools/ci_toolkit/file_tools.py` | Create | CI-cached file reads |
| `server/routers/code_intelligence.py` | Create | CI API endpoints |
| `server/routers/sandboxes.py` | Modify | Enhanced sandbox routes |
| `server/app_factory.py` | Modify | Wire services + routers |
| `tools/factory.py` | Modify | Register new factories |
| `tools/__init__.py` | Modify | Register CIToolkit |

---

## Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| tree-sitter dependency may not be available | Fall back to Python `ast` module (already exists in `services/lsp/`); make tree-sitter optional |
| Lock ordering complexity from synthetic-os | Preserve same 4-group lock ordering invariant; document in module docstring |
| Large port (~12K lines CI service) | Port incrementally: types → tree_cache → symbol_index → arbiter → gateway → service |
| Daytona SDK version differences | Pin SDK version in pyproject.toml; existing client.py already handles this |
| CI service memory usage for large repos | Keep max-items limits from synthetic-os (10K files, configurable indexing scope) |
| Breaking existing DaytonaToolkit API | New tools are additive; existing 6 tools unchanged; CI integration is opt-in |

---

## Dependencies to Add

```toml
# pyproject.toml — optional
tree-sitter = {version = ">=0.21", optional = true}
tree-sitter-python = {version = ">=0.21", optional = true}
tree-sitter-javascript = {version = ">=0.21", optional = true}
tree-sitter-typescript = {version = ">=0.21", optional = true}
```

---

## Suggested Implementation Order

1. **Step 1** (Sandbox Service) — foundation, no dependencies on CI
2. **Step 2** (Code Intelligence Service) — core runtime, depends only on tree-sitter/ast
3. **Step 3** (Enhanced Daytona Toolkit) — depends on Steps 1+2
4. **Step 4** (CI Toolkit) — depends on Step 2
5. **Step 5** (API Routes) — depends on Steps 1+2
6. **Step 6** (Factory Wiring) — depends on Steps 3+4
7. **Step 7** (DB Persistence) — optional, can be deferred
