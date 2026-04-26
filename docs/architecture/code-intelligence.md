# Code Intelligence

The Code Intelligence subsystem provides Python-focused code querying, safe file
mutation, and audited shell command execution for local and sandbox workspaces.

Current supported semantic language: Python.

Removed/obsolete pieces: legacy query-router/backend-protocol modules,
`indexing/tree_cache.py`, TypeScript/JavaScript LSP probing, and tree-sitter
based symbol extraction.

## Architecture Overview

```
Client tools
  |
  v
CodeIntelligenceService
  |-- SymbolIndex           Python AST symbol index
  |-- LspClient             Python/Jedi semantic queries
  |-- MutationService       write/edit/delete/move commits
  |-- WriteCoordinator      OCC, locks, merge, rollback, refresh
  |-- ContentManager        local/sandbox file I/O
  `-- AuditedCommandExecutor -> OverlayAuditor -> overlay/run.py
```

`CodeIntelligenceService` is intentionally a facade. New behavior should usually
live in one of the owner modules above rather than growing
`service.py`.

## Responsibility Owners

- `service.py`: per-sandbox wiring, initialization, sandbox rebinding,
  and public facade methods.
- `indexing/symbol_index.py`: background workspace indexing and refresh after
  edits.
- `indexing/symbol_extractor.py`: Python AST extraction for functions, classes,
  methods, and assignments.
- `language_server/client.py`: Python/Jedi subprocess queries, readiness checks, caching,
  and diagnostics.
- `mutations/mutation_service.py`: typed write/edit/delete/move APIs.
- `mutations/write_coordinator.py`: operation-level OCC, sorted per-file locking,
  stale-edit merge fallback, rollback, index refresh, and LSP invalidation.
- `mutations/content_manager.py`: workspace-scoped local and sandbox file I/O.
- `overlay/command_executor.py` and `overlay/auditor.py`: audited
  shell command execution.
- `telemetry.py`: status and telemetry response shaping.

## Runtime Injection Contract

Daytona-backed tools use `sandbox.workspace.ensure_code_intelligence_runtime(...)`
as the shared injection boundary. The helper owns runtime metadata:

- `daytona_sandbox`: active Daytona sandbox handle.
- `repo_root`: canonical repository root inside the sandbox.
- `exec_cwd`: shell execution cwd, defaulting to `repo_root`.
- `ci_workspace_root`: optional code-intelligence root override.
- `ci_service`: the per-sandbox `CodeIntelligenceService`.

Write-capable tools call typed service APIs directly:

- `svc.write_file(specs)`
- `svc.edit_file(specs)`
- `svc.delete_file(paths)`
- `svc.move_file(specs)`
- `svc.cmd(sandbox, command, ...)`

Each typed mutation call is one OCC boundary. Shell-style commands run through
the overlay audit path and commit tracked file changes through the same
`WriteCoordinator`.

## Path Rules

`ContentManager` resolves relative paths under the configured workspace root and
rejects relative traversal that escapes that root. Absolute paths are preserved
because Daytona tools commonly pass canonical sandbox paths.

## Symbol Indexing

`SymbolIndex` starts a background build on first `ensure_initialized()` or
`ensure_built()` call.

Workflow:

1. Discover Python files under the workspace root, skipping cache/build
   directories from `constants.SKIP_DIRECTORIES`.
2. For sandbox workspaces, batch-download files when possible; fall back to
   individual reads.
3. Extract Python symbols with `ast`.
4. Store symbols in a thread-safe per-file map with a generation counter.
5. Refresh or remove a single file after each committed mutation.

The index intentionally ignores non-Python files.

## LSP Integration

`LspClient` runs Python semantic queries through Jedi. It supports:

- `goto_definition(file_path, line, character)`
- `find_references(file_path, line, character)`
- `find_references_many(requests)`
- `hover(file_path, line, character)`
- `diagnostics(file_path)`
- `ensure_ready(install_missing=False, languages=("python",))`

Local queries run a Python subprocess. Sandbox queries ship the Python script
through `sandbox.process.exec()`. Results are JSON-decoded into the shared
`types.py` dataclasses.

The client keeps:

- an LRU query cache keyed by operation/file/position,
- an in-flight query map so concurrent identical requests share work,
- a small line cache used to adjust column `0` to the actual symbol name on
  `def` and `class` lines,
- readiness state for the Python/Jedi backend.

## Mutation Workflow

Typed mutation APIs build `OperationChange` entries and submit them to
`WriteCoordinator`.

```
Tool request
  -> MutationService reads plan-time bases through ContentManager
  -> OperationChange[] captures base_content, base_hash, final_content
  -> WriteCoordinator locks sorted paths
  -> resolve exact-base, create, delete, or stale-edit merge
  -> apply writes/deletes
  -> record edit generation
  -> refresh SymbolIndex and invalidate LspClient
```

Important policies:

- Create with `overwrite=False` requires the target to be absent.
- Delete requires the current hash to match the captured base hash.
- Whole-file strict rewrites abort on any drift.
- Non-strict stale edits may merge only when the changed line window does not
  overlap current changes.
- One operation commits all files or none.

## Audited Shell Commands

`svc.cmd(...)` routes through `OverlayAuditor`:

1. Build a dangling Git snapshot of the live workspace.
2. Upload `overlay/run.py` as `overlay_run.py` and run it in a fresh rootless
   overlay mount.
3. Run the user command under the merged workspace view.
4. Classify upperdir changes:
   - gitignored regular-file writes direct-merge into the live workspace,
   - non-ignored writes emit NDJSON for OCC commit,
   - `.git` writes and unsupported overlay cases reject.
5. Commit non-ignored changes through `OverlayCommandCommitter` and
   `WriteCoordinator`.
6. Return the downstream shell result shape with changed paths, ambient paths,
   git commit status, conflict metadata, warnings, and snapshot timings.

This keeps shell, test, and build commands on one audited mutation path.

## Telemetry

`service.status()` and `service.get_telemetry()` aggregate:

- symbol index size, generation, and indexed file count,
- LSP connected state, query count, and cache hits,
- Arbiter active locks, total edits, and conflicts,
- overlay counters.

## Cleanup Notes

High-value simplification targets:

- Split `LspClient` into a small facade, query cache, Python/Jedi backend, and
  script transport.
- Split `WriteCoordinator` into lock acquisition, change resolution, checked
  apply, and commit recording helpers shared by single and batch operations.
- Split `ContentManager` into local, process-backed sandbox, and Daytona FS
  transport adapters.
