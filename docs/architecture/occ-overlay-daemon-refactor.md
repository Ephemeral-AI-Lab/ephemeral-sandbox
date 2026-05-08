# OCC + Overlay Two-Module Refactor Plan

**Status:** Superseded reference draft. The later sandbox API/runtime execution
plan has been retired from the docs tree. This document is retained for the
OCC/Overlay responsibility split research and uses the updated naming below:
`sandbox/daemon/rpc/dispatcher.py` is the generic in-sandbox
guarded service; OCC and Overlay own sandbox-side handlers plus bundled
`setup.sh`. The refactored domain modules are only `sandbox/occ/` and
`sandbox/overlay/`; `sandbox/daemon/` is daemon/server support.
**Author:** session 2026-05-03
**Scope:** ~7K LoC, ~35 files across `backend/src/sandbox/code_intelligence/`, `backend/src/sandbox/api/`, `backend/src/sandbox/daemon/` (new), and tests under `backend/tests/test_sandbox/`.
**Companion doc:** `plugins-refactor.md` covers the query-surface replacement (direct LSP plugin tools). Both refactors land together; query-side deletions are listed here for completeness but executed under the plugins plan.

## 0. Motivation

Today `sandbox/code_intelligence/` is an over-broad umbrella that bundles two unrelated guardrails plus a query surface:

1. **Write/edit guardrail** — OCC arbiter, content manager, write coordinator, edit history ledger.
2. **Command-execution guardrail** — overlay, command committer, process exec.
3. ~~Code intelligence queries~~ — moved out under `plugins-refactor.md`.

The two guardrails serve different chokepoints (file edits vs sandbox cmd execution) and deserve to be peers, not siblings under a misleading `code_intelligence/` parent. The umbrella name is misleading once queries leave.

## 0.1 Pre-step: collapse `move_file` / `remove_file` into shell

Before the OCC/Overlay/runtime move starts, delete the two dedicated tools (`tools/sandbox_toolkit/move_file.py`, `tools/sandbox_toolkit/remove_file.py`) and route those operations through `svc.cmd` (`mv`, `rm`). The overlay commit path already funnels every non-gitignored upperdir change through OCC via `OverlayCommandCommitter`, so audit/ledger coverage is preserved.

Doing this *before* the package move keeps OCC's external surface (§2.1) from inheriting verbs we're about to delete, and removes a layer of API plumbing (`AuditedSandboxApi.{move,remove}_file`, old daemon RPC handlers, `MoveSpec`-batching code in `mutation_service`) that the refactor would otherwise have to relocate.

**Files deleted in pre-step:**

- `backend/src/tools/sandbox_toolkit/move_file.py`
- `backend/src/tools/sandbox_toolkit/remove_file.py`

**Call sites and downstream code to remove or update in the same change set:**

- `tools/sandbox_toolkit/registry.py` — drop the two imports and registrations.
- `tools/sandbox_toolkit/shell.py:174` and `tools/sandbox_toolkit/_shell_prehooks.py:63` — update guidance strings (no longer steer agents to `remove_file` / `move_file`).
- `tools/submission/hooks/request_complex_task_before_edit_gate.py:19-20` — drop the two tool names from the gate's covered set, or expand the gate to cover `shell` if equivalent coverage is desired.
- `agents/profile/helper/resolver.md`, `agents/profile/main/entry_executor.md`, `agents/profile/main/generator_executor.md` — strip `remove_file` / `move_file` from each agent's tool list.
- `backend/tests/unit_test/test_engine/eval_agent_support.py:384-385` — strip the two tool names from the eval allowlist.
- `sandbox/api/audited_sandbox_api.py:134-162` — delete `remove_file` / `move_file` methods.
- `sandbox/api/sandbox_api.py:49-53` — drop the corresponding protocol methods.
- `sandbox/api/audit.py` — delete `submit_remove_request` / `submit_move_request` and the `RemoveFileRequest` / `MoveFileRequest` / `RemoveFileResult` / `MoveFileResult` models from the public `sandbox.api` model contract if no other caller remains.
- `sandbox/code_intelligence/service.py:270-277` — delete `move_file` (and `delete_file` if unused).
- `sandbox/code_intelligence/mutations/mutation_service.py:282-334` — delete `move_file`; remove `op == "move"` / `op == "delete"` branches in `_commit_specs_direct` once verified unused.
- `sandbox/code_intelligence/backends/{protocol.py:88, in_process.py:286-293}` — drop `move_file` from backend protocol + impl.
- `sandbox/code_intelligence/daemon/handlers.py:337-398` — delete `handle_move_file` and remove `"move_file"` from the dispatch table; same for `delete_file` if present.
- `sandbox/code_intelligence/daemon/client.py:414-422` — delete the daemon-client `move_file` / `delete_file` shims.
- `sandbox/code_intelligence/core/types.py:169` — delete `MoveSpec` once mutation_service no longer references it.

**Behavioral consequences (accepted, not mitigated):**

- Agents lose the structured `dst_exists | not_found | aborted_version | aborted_overlap | aborted_lock` enum and read shell stderr / `audit_conflict_reason` instead.
- `mv` clobbers by default; agents must use `mv -n` if non-overwrite is desired.
- `rm -rf` of folders is the agent's responsibility — no `is_folder=True` typed switch.
- Moves/removes of gitignored paths stop hitting OCC (overlay direct-merges gitignored writes); ledger sees only gitinclude-tracked paths.
- Per-op cost rises from ~OCC-only to ~overlay+commit (~1.1s end-to-end vs ~0.65s commit-only).

These are the trade-offs we are choosing in exchange for OCC surface reduction and one fewer tool family.

## 1. End-state architecture

Two independent sandbox-level modules plus the daemon support they share:

```
backend/src/
└── sandbox/
    ├── daemon/     # in-sandbox RPC dispatcher, handlers, services
    ├── host/       # host-side bundle deploy, RPC client, ops
    ├── occ/        # was code_intelligence/mutations + ledger
    └── overlay/    # was code_intelligence/overlay
```

`sandbox/code_intelligence/` ceases to exist.

### 1.1 Naming decisions

- **No `guardrail/` umbrella.** OCC and Overlay are two separate modules.
- The OCC/Overlay split is the contract: edits go through OCC, shell goes
  through Overlay to produce an `OverlayCapture`, then OCC applies that capture.
  No third mutation path.
- **Two modules, one shared daemon.** Count `sandbox/occ/` and
  `sandbox/overlay/` as the refactored modules. `sandbox/daemon/` exists
  because both modules need a deployed in-sandbox dispatcher and shared
  services; `sandbox/host/` owns bundle upload and host-side RPC.
- **`dispatcher.py`, not peer-specific daemon logic.** `sandbox/daemon/rpc/dispatcher.py`
  is a generic OP_TABLE dispatcher: decode request, validate, lookup handler,
  run handler, encode result. It does not hardcode one branch for each
  OCC or Overlay request.
- **No local guardrail clients.** Public tools submit guarded requests through
  `sandbox/host/rpc/client.py` into `daemon/rpc/dispatcher.py`. OCC and Overlay
  behavior is selected by sandbox-side handlers, not by process-local
  guardrail registries.
- **Host deploy owns bundling.** `sandbox/host/deploy/bundle.py` builds the
  daemon bundle; in-sandbox behavior is registered through daemon handlers.

### 1.2 OCC chokepoint

Every file edit converges on a single OCC class. `mutation_service.py`, `arbiter.py`, `content_manager.py`, `patcher.py`, and `write_coordinator/` collapse into OCC internals. External callers see one entry point.

### 1.3 Overlay chokepoint

Every `service.cmd()` routes through `sandbox/overlay/`. Existing overlay logic relocates with minimal change — the chokepoint is already in place; the move just makes it visible.

## 2. Sandbox-Side Packages

### 2.1 `sandbox/occ/`

Single OCC class is the chokepoint. Internals:

```
sandbox/occ/
├── __init__.py
├── setup.sh                       # OCC setup bundled by host/deploy if present
├── bootstrap.py                   # registers setup.sh + server handlers
├── handlers/                      # thin server op adapters
│   ├── write.py
│   ├── edit.py
│   └── apply_changeset.py
├── overlay_capture.py             # OverlayCapture → OCC changes + apply helpers
├── arbiter.py
├── content_manager.py
├── patcher.py
├── write_coordinator/             # unchanged structure, relocated
├── ledger_store.py                # was daemon/ledger_store.py — edit history
├── types.py                       # was core/types.py (EditSpec, WriteSpec, MoveSpec, OperationResult)
├── hashing.py                     # was core/hashing.py
└── engine.py                      # concrete OCC composition root
```

External API: public write/edit verbs route through `daemon/rpc/dispatcher.py` to
sandbox-side OCC handlers. Move and delete verbs are removed from the external
surface (see §0.1) — `mv` / `rm` flow through shell and commit by applying an
`OverlayCapture` through OCC. Internally, overlay commits can still produce
delete changes consumed by the OCC merge path; that is not a public OCC method.

### 2.2 `sandbox/overlay/`

Mostly relocation:

```
sandbox/overlay/
├── __init__.py
├── setup.sh                       # overlay setup bundled by host/deploy if present
├── bootstrap.py                   # registers setup.sh + server handlers
├── handlers/
│   └── run.py                     # in-sandbox overlay implementation
├── capture.py                     # pure upperdir capture
├── command_executor.py            # temporary execution shim only until Step 6 split
├── process_exec.py                # low-level process execution
├── daemon_exec.py                 # renamed from daemon_local.py if a daemon-local path remains
├── results.py
├── types.py
└── engine.py                      # OverlayEngine Protocol
```

External API: public shell routes through `daemon/rpc/dispatcher.py`, receives an
`OverlayCapture` inside the sandbox runtime, and applies that capture through
`occ.overlay_capture`. Shell no longer has a read-only `raw_exec` bypass and no
longer falls back to the removed live-root overlay runtime. Overlay never
imports OCC and never classifies gitignored vs gitincluded paths itself.

No `auditor.py` remains in the target overlay package. The audit name implied
policy ownership. The overlay side is capture-only; legacy `gitinclude_*` /
`gitignore_*` response projection belongs outside `overlay/` during the
compatibility window and disappears when public `sandbox.api.shell` takes over.

### 2.3 Shared `sandbox/daemon/` Support

The old daemon code moves out from under `code_intelligence/` and becomes the
sandbox daemon layer. `rpc/dispatcher.py` is the in-sandbox guarded service. It
is generic: request decoding, validation, OP_TABLE lookup, result encoding, and
structured errors. OCC and Overlay behavior is registered by handler modules,
not hardcoded in the dispatcher. This package is daemon support for the
two-module refactor, not a third peer module.

```
sandbox/daemon/
├── __init__.py
├── __main__.py
├── rpc/
│   ├── server.py
│   └── dispatcher.py
├── handlers/
│   ├── edit.py
│   ├── read.py
│   ├── shell.py
│   ├── workspace.py
│   └── write.py
├── services/
│   ├── occ_backend.py
│   ├── shell_runner.py
│   └── workspace_server.py
├── overlay_shell/
│   └── cli.py
```

There is no generic public daemon client and no local OCC/Overlay client
registry. Public agent tools still import only
`sandbox.api.{shell,read,write,edit}`; those modules call `daemon/rpc/dispatcher.py`.

DELETED from the old daemon: `index_store.py`, all symbol-query RPC handlers,
all symbol-related wire types. (See `plugins-refactor.md` for the query-side
replacement.)

### 2.4 Shared path utilities

`code_intelligence/core/path_utils.py` and `core/constants.py` → `sandbox/_paths.py` (single util module shared by OCC, Overlay, and daemon code). Anything occ-specific lives in `occ/`; anything overlay-specific lives in `overlay/`.

## 3. Deletions

### 3.1 Code

- `sandbox/code_intelligence/service.py` (old code-intelligence facade — replaced by `OCC` + `Overlay` separately)
- `sandbox/code_intelligence/registry.py` (replaced by provider adapter lookup plus peer clients, not by a new shared code-intelligence facade)
- `sandbox/code_intelligence/__init__.py`, `backends/` — all relocated or deleted
- `sandbox/code_intelligence/` (the directory itself, after everything inside has moved or been deleted)
- Query-side deletions (`indexing/`, `language_server/`, `daemon/index_store.py`) — owned by `plugins-refactor.md`.

### 3.2 API surface

- `sandbox/api/code_intelligence_api.py` (entire file)
- `sandbox/api/code_intelligence_impl.py` (entire file)
- Query-related types in the public `sandbox.api` model contract (`SymbolInfo`, `ReferenceInfo`, `HoverResult`, `Diagnostic`, etc. — relocated only if still referenced; otherwise deleted)
- New: `sandbox/api/occ_api.py` and `sandbox/api/overlay_api.py` if external HTTP/RPC surface is needed

### 3.3 Tests

- `backend/tests/test_sandbox/test_code_intelligence/*` — relocate to `test_occ/` and `test_overlay/`, or delete
- `backend/tests/test_sandbox/test_daemon_*.py` — move under runtime tests and update for `server.py` dispatch
- Indexing/query test deletions are owned by `plugins-refactor.md`.

### 3.4 Compatibility shims

This earlier draft originally assumed no shims. Public `sandbox.api.*` verbs now
own the caller-facing sandbox API surface.

## 4. External call sites to rewrite

Found via grep:

- `sandbox/lifecycle/workspace.py` — uses `service.symbol_index`, `service.lsp_client`, etc. (Replaced with OCC + direct plugin-tool lookup wired per `plugins-refactor.md`.)
- `sandbox/api/code_intelligence_api.py` — DELETE
- `sandbox/api/code_intelligence_impl.py` — DELETE
- public `sandbox.api` model contract — strip query types
- `sandbox/api/audit.py` — references mutations module → route through OCC
- `backend/tests/test_sandbox/test_code_intelligence/*` — relocate or delete
- `backend/tests/test_sandbox/test_daemon_*.py` — move under runtime tests and update for `server.py` dispatch

## 5. Sequenced execution

This plan picks up after `plugins-refactor.md` step 0 proves sandbox-hosted
basedpyright connectivity and steps 1–5 author/smoke-test the direct LSP plugin
tools, so that `lifecycle/workspace.py` can swap to plugin-tool lookup in one
pass without an intermediate broken state.

```
0. Pre-step: collapse move_file / remove_file into shell (per §0.1)
   - Delete tools/sandbox_toolkit/{move_file.py, remove_file.py}
   - Update tools/sandbox_toolkit/registry.py, shell.py, _shell_prehooks.py
   - Update tools/submission/hooks/request_complex_task_before_edit_gate.py
   - Strip remove_file / move_file from agent.md files and backend/tests/unit_test/test_engine/eval_agent_support.py
   - Delete AuditedSandboxApi.{move,remove}_file + the SandboxApi protocol pair
   - Delete audit.submit_{move,remove}_request and unreferenced request/result models
   - Delete service.move_file, mutation_service.move_file, MoveSpec, backends move_file,
     daemon handle_move_file, daemon client move_file (and delete_file equivalents
     if no internal caller remains)
   - make test + ruff check; iterate to green before starting step 1

1. Move sandbox/code_intelligence/mutations/ → sandbox/occ/
   - Collapse arbiter + patcher + content_manager + mutation_service into OCC class
   - Keep write_coordinator/ and edit_history_ledger.py as internals

2. Move sandbox/code_intelligence/overlay/ → sandbox/overlay/
   - Verify Overlay.cmd is the only entry; no other module invokes shell

3. Move sandbox/code_intelligence/core/ types
   - EditSpec/WriteSpec/MoveSpec/OperationResult → sandbox/occ/types.py
   - Path normalizers → sandbox/_paths.py
   - hashing → sandbox/occ/hashing.py

4. Move sandbox/code_intelligence/daemon/ → sandbox/daemon/
   - Replace command.py's switch with daemon/rpc/dispatcher.py
   - Keep server.py generic: request envelope → OP_TABLE lookup → handler → result envelope
   - Host bundle deploy lives under sandbox/host/deploy/
   - DELETE: index_store.py, symbol-query handlers, symbol wire types
   - ledger_store.py moves to sandbox/occ/ledger_store.py

5. Add sandbox daemon dispatch and setup scripts
   - public sandbox API tool modules route guarded requests to daemon/rpc/dispatcher.py with one adapter.exec
   - sandbox/occ/setup.sh and sandbox/overlay/setup.sh are registered by peer bootstrap.py files
   - Do not add local OCC/Overlay client registries or a public daemon client

6. Delete old backends and registries
   - DELETE old code_intelligence/backends/
   - Old code_intelligence/registry.py and service.py DELETED
   - Public agent tools route through sandbox.api.*; API modules delegate to daemon/rpc/dispatcher.py

7. Mass deletions
   - api/code_intelligence_api.py + code_intelligence_impl.py
   - Query types from api/models.py
   - Tests targeting deleted surface (coordinated with plugins-refactor.md §4)

8. Rewrite call sites; delete the temporary shim after public verbs own callers
   - sandbox/lifecycle/workspace.py: replace service.symbol_index/lsp_client refs with OCC + plugin-tool lookup
   - api/audit.py: route through OCC
   - Any remaining tools/* references: rewrite or delete

9. Clean up tests
   - Relocate OCC tests to backend/tests/test_sandbox/test_occ/
   - Relocate overlay tests to backend/tests/test_sandbox/test_overlay/
   - Delete indexing/query tests (per plugins-refactor.md)

10. make test + ruff check; iterate to green

11. Documentation
    - docs/architecture/code-intelligence-in-sandbox-daemon/ → docs/architecture/occ-overlay/
    - Rewrite phase-08 implementation report to reflect new architecture

12. Final verification: code_intelligence/ directory empty → `git rm -r` it.
```

## 6. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Runtime server dispatch breaks RPC compatibility | Same wire protocol shape, but delivery routes through `server.py` and peer clients. Update tests for the new dispatch surface. |
| External callers depending on mutation/overlay internals | All call sites enumerated in §4; rewritten in same change set. |
| Lost edit history during move | `ledger_store.py` relocates as-is; no schema change. |
| Tests fail to delete cleanly | Each test file inspected; deletion is line-item, not bulk. |
| `lifecycle/workspace.py` rewrite blocked on plugin-tool lookup not being ready | Sequencing: plugin-tool half lands first; this plan starts at step 1 only after plugin smoke test passes. |

## 7. Out of scope

- Anything plugin-related (see `plugins-refactor.md`).
- Multi-daemon-process topologies (still one runtime server path per sandbox).
- OCC/Overlay sharing a base `Chokepoint` interface (deferred — duck-typed peers for v1).

## 8. Open questions deferred to execution

- Exact location of `sandbox/_paths.py` (root of `sandbox/` vs a tiny `sandbox/util/` package).
- Whether `OCC` and `Overlay` should share a base `Chokepoint` interface or remain duck-typed peers.
- Exact server registration bootstrap list: `server.py` may import known peer bootstraps to populate `OP_TABLE`, but request dispatch must remain table-driven.

These do not change the plan shape; resolve in the relevant step.
