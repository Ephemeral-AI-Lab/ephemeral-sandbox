# Step 5 — Slice 4 — OCC peer relocation

**Goal.** Move OCC under `sandbox/occ/`; add OCC's `client.py` route point and `setup.sh`; register its setup and handlers with the runtime at import time. Wire `edit_pipeline` (multi-edit OCC planning + atomic commit) and `write_pipeline` (OCC write planning + atomic commit) end-to-end inside the sandbox. Both are reachable through `runtime/server.py` but **not yet exposed via `sandbox.api`** — that's Slice 6.

**Depends on.** Step 4 / Slice 3.

## Files

### Target package layout

The OCC package should be structured by responsibility, not as a flat dump of
the old `code_intelligence/mutations/` package. Root-level files are only
entrypoints, contracts, and wire helpers; implementation modules live under
focused subpackages.

```
sandbox/occ/
    __init__.py
    setup.sh
    bootstrap.py              # registers setup.sh + server handlers
    client.py                 # host-side typed OCC request client
    engine.py                 # concrete engine boundary
    types.py                  # OCC request/result dataclasses
    wire.py                   # JSON serialization for OCC server requests

    handlers/                 # server op adapters only, no OCC policy
        __init__.py
        write.py
        edit.py
        apply_changeset.py

    operations/               # high-level write/edit operation planning
        __init__.py
        service.py            # renamed former MutationService

    content/                  # workspace content I/O and exact-base apply
        __init__.py
        manager.py
        hashing.py
        path_utils.py

    commit/                   # OCC commit/merge pipeline
        __init__.py
        coordinator.py
        resolver.py
        merge.py
        metrics.py
        models.py
        results.py

    changeset/                # overlay UpperChange -> OCC/direct-merge policy
        __init__.py
        apply.py
        types.py

    patching/
        __init__.py
        patcher.py

    state/                    # coordination and ledger state
        __init__.py
        arbiter.py
        edit_history_ledger.py
        ledger_store.py
        constants.py
```

### Move
- `backend/src/sandbox/code_intelligence/mutations/mutation_service.py` →
  `backend/src/sandbox/occ/operations/service.py`. Rename the concrete class
  away from `MutationService` (for example `OCCOperationService`) so the new
  package no longer carries the old umbrella term.
- `backend/src/sandbox/code_intelligence/mutations/content_manager.py` →
  `backend/src/sandbox/occ/content/manager.py`.
- `backend/src/sandbox/code_intelligence/core/hashing.py` →
  `backend/src/sandbox/occ/content/hashing.py`.
- `backend/src/sandbox/code_intelligence/core/path_utils.py` →
  `backend/src/sandbox/occ/content/path_utils.py`.
- `backend/src/sandbox/code_intelligence/mutations/write_coordinator/` →
  `backend/src/sandbox/occ/commit/`.
- `backend/src/sandbox/code_intelligence/mutations/merge.py` →
  `backend/src/sandbox/occ/commit/merge.py`.
- `backend/src/sandbox/code_intelligence/mutations/changeset.py` →
  `backend/src/sandbox/occ/changeset/apply.py`, with `ChangesetResult` and
  `UpperChangeLike` split into `occ/changeset/types.py`.
- `backend/src/sandbox/code_intelligence/mutations/patcher.py` →
  `backend/src/sandbox/occ/patching/patcher.py`.
- `backend/src/sandbox/code_intelligence/mutations/arbiter.py` →
  `backend/src/sandbox/occ/state/arbiter.py`.
- `backend/src/sandbox/code_intelligence/mutations/edit_history_ledger.py` →
  `backend/src/sandbox/occ/state/edit_history_ledger.py`.
- `backend/src/sandbox/code_intelligence/daemon/ledger_store.py` →
  `backend/src/sandbox/occ/state/ledger_store.py`.
- OCC-owned constants from `backend/src/sandbox/code_intelligence/core/constants.py`
  (`ARBITER_*`, `PATCHER_MAX_DIFF_SIZE`) →
  `backend/src/sandbox/occ/state/constants.py`. Query/index constants stay with
  the query-side migration work.
- OCC request/result dataclasses from
  `backend/src/sandbox/code_intelligence/core/types.py` →
  `backend/src/sandbox/occ/types.py`. In this checkout, the current file is
  OCC-owned (`EditResult`, `OperationChange`, `OperationResult`, `WriteSpec`,
  `EditSpec`).
- `backend/src/sandbox/code_intelligence/daemon/wire.py` →
  `backend/src/sandbox/occ/wire.py`.

### Add
- `backend/src/sandbox/occ/__init__.py` — light package marker; do not import
  the host client or heavy engine objects here.
- `backend/src/sandbox/occ/client.py` — `OCCClient`, the host-side typed route for public OCC write/edit requests. It serializes the request, invokes `runtime/server.py` through exactly one adapter exec, and returns typed OCC/result objects.
- `backend/src/sandbox/occ/setup.sh` — OCC setup submitted to the runtime/daemon by `occ/bootstrap.py` after bundle upload.
- `backend/src/sandbox/occ/engine.py` — concrete in-sandbox OCC composition root.
- `backend/src/sandbox/occ/bootstrap.py` — registers `setup.sh`, bundle contributions, and OCC handlers at import time.
- `backend/src/sandbox/occ/handlers/` — thin server op adapters only:
  `write`, `edit`, `apply_changeset`.

### Modify
- `sandbox/runtime/server.py` — import `sandbox.occ.bootstrap` / handlers so OCC ops register at import time. Server dispatch remains `OP_TABLE`-based; no per-OCC branch is added.
- `sandbox/runtime/pipelines.py`:
  - `edit_pipeline`: take a list of edits, plan them through OCC, then commit once. Atomic — partial apply rolls back on conflict via OCC arbiter.
  - `write_pipeline`: drive OCC `write` then `commit` in one in-sandbox process; one wire trip total.
- `sandbox/runtime/bundle.py` — include `sandbox/occ/**/*.py` and
  `sandbox/occ/setup.sh` in the runtime bundle. Do not keep bundling the old
  `code_intelligence/mutations/` tree after the import migration is complete.
- Temporary compatibility callers under `sandbox/code_intelligence/`,
  `sandbox/api/audit.py`, `sandbox/lifecycle/commit.py`,
  `sandbox/runtime/legacy_command_client.py`, and `tools/core/` should import
  from `sandbox.occ.*` while those legacy surfaces still exist.
- Rename OCC-internal `apply_edit` to `apply` so it does not shadow the public
  `edit` verb. Public `edit` / `write` verbs land in Slice 6.

### Delete
- `backend/src/sandbox/code_intelligence/mutations/` (after move; this is the slice that retires the old location).
- `backend/src/sandbox/code_intelligence/core/hashing.py` after callers import
  `sandbox.occ.content.hashing`.
- `backend/src/sandbox/code_intelligence/daemon/ledger_store.py` and
  `daemon/wire.py` after callers import `sandbox.occ.state.ledger_store` and
  `sandbox.occ.wire`.
- `backend/src/sandbox/code_intelligence/mutations/mutation_results.py`.
  Inline these small planning-failure helpers into `occ/operations/service.py`
  instead of preserving another file.
- The old snapshot undo stack. Runtime OCC keeps atomic commit rollback inside
  `WriteCoordinator`; reusable user-facing undo state is not carried forward.

Keep temporary re-export modules only where needed to keep this slice green;
remove those shims in Slice 7 when `code_intelligence/` is deleted.

## Implementation tasks

1. Create the target `sandbox/occ/` package and move files into the
   responsibility-based layout above. Do not land an intermediate flat
   `occ/{changeset,arbiter,content_manager,...}.py` dump.
2. Rename old umbrella language while moving:
   - `MutationService` → `OCCOperationService` (or a similarly direct name).
   - `write_coordinator/` → `commit/`.
   - `content_manager.py` → `content/manager.py`.
   - `changeset.py` → `changeset/apply.py` plus `changeset/types.py`.
3. Extract OCC request/result types into `sandbox/occ/types.py`. If anything
   still imports from `code_intelligence/core/types.py`, leave a temporary
   re-export there until Slice 7. Do not leave new production code importing
   from the old core path.
4. Move `hashing.py`, `path_utils.py`, and OCC-owned constants with the code
   that consumes them. The new OCC package should not depend on
   `sandbox.code_intelligence.core.*`.
5. Move `daemon/wire.py` to `occ/wire.py` and update
   `runtime/legacy_command_client.py` plus temporary compatibility callers to
   use that path.
6. Define `LocalOCCEngine` as the concrete composition root for handlers and
   pipelines. Do not add a protocol until there is a second implementation.
7. Rename OCC verbs and audit every internal caller. Add a temporary lint check
   that grep-fails on `apply_edit` — remove the check at end of slice once zero
   hits.
8. Implement `OCCClient`. It owns all host-side OCC request routing and is the
   only place outside `runtime/` that constructs OCC server envelopes.
   It should expose typed methods for the operations that later back public
   `sandbox.api.write/edit`, plus `apply_changeset` for overlay composition.
   It does not import Overlay.
9. Add `occ/setup.sh` and make `occ/bootstrap.py` register it with
   `runtime/setup_orchestrator.py`. Keep setup idempotent; it may initialize
   ledger directories or OCC-local state, but it must not run shell/user
   commands.
10. Register OCC handlers in `OP_TABLE` at module import time (via
    `sandbox/occ/handlers/__init__.py`). Handler modules call into OCC
    internals; they do not own policy themselves.
11. Implement `edit_pipeline` and `write_pipeline` inside
    `runtime/pipelines.py`. They run in-process inside the sandbox, dispatched
    by `server.py`.
12. Update `runtime/bundle.py` and bundle tests so the deployed runtime
    contains `sandbox/occ/` including `setup.sh`; remove old
    `code_intelligence/mutations/` from the bundle once imports are migrated.
13. Run a package-boundary grep:
    - `grep -r "sandbox.code_intelligence" backend/src/sandbox/occ/` returns
      zero hits.
    - `grep -r "sandbox.overlay" backend/src/sandbox/occ/` returns zero hits.
    - `grep -r "from sandbox.code_intelligence.mutations" backend/src/`
      returns zero production hits.

## Tests

- All existing OCC mutation tests pass at the new path.
- New `test_sandbox/test_occ/test_package_structure.py`:
  - root `sandbox/occ/` contains only entrypoint/contract/wire files and the
    expected subpackages.
  - no import in `sandbox/occ/` reaches into `sandbox.code_intelligence.*`.
  - no import in `sandbox/occ/` reaches into `sandbox.overlay.*`.
- New `test_sandbox/test_occ/test_client.py`:
  - `OCCClient` performs exactly one adapter exec per request.
  - `OCCClient` serializes requests to `runtime/server.py` rather than
    reaching into handlers directly.
  - `OCCClient` does not import `sandbox.overlay`.
- New `test_sandbox/test_occ/test_bootstrap.py`:
  - `occ/bootstrap.py` registers `occ/setup.sh` with the setup orchestrator.
  - repeated setup registration/execution is idempotent.
- New `test_sandbox/test_occ/test_pipelines.py`:
  - `edit_pipeline` atomic across N edits → exactly one commit.
  - `write_pipeline` write+commit in one server call → one wire trip.
  - Conflict path: `edit_pipeline` returns `ConflictInfo(reason="patch_failed", path=...)` and the OCC ledger is unchanged.
- Bundle test update: extracted runtime bundle includes `sandbox/occ/setup.sh`
  and required OCC modules, and no longer includes
  `sandbox/code_intelligence/mutations/`.

## Exit criteria

- Build / ruff / tests green.
- `code_intelligence/mutations/` no longer exists.
- `sandbox/occ/` follows the responsibility-based layout in this document.
- `grep -r "sandbox.code_intelligence" backend/src/sandbox/occ/` returns zero
  hits.
- `grep -r "sandbox.overlay" backend/src/sandbox/occ/` returns zero hits.
- `sandbox/occ/client.py` is the only host-side route for OCC server
  requests; `sandbox/api.write/edit` are not wired yet in this slice.
- `sandbox/occ/setup.sh` is registered through `occ/bootstrap.py`.
- `sandbox/occ/handlers/` contains request adapters only; core OCC policy
  remains in `operations/`, `content/`, `commit/`, `changeset/`, `patching/`,
  and `state/`.
- `runtime/bundle.py` deploys `sandbox/occ/` and `occ/setup.sh`; it does not
  deploy the retired `code_intelligence/mutations/` path.
- The two new pipelines are dispatch-reachable through `server.py`; `sandbox.api` does not yet expose them.
- Grep confirms the old apply name and snapshot-undo route are gone from
  `backend/src/`.

## Risks

- A renamed verb leaves a stale internal reference. Mitigation: temporary lint check + full grep audit.
- The move becomes a cosmetic directory shuffle while OCC still imports
  `sandbox.code_intelligence.*`. Mitigation: package-boundary grep and
  `test_package_structure.py` block the slice.
- Over-structuring creates tiny one-line modules. Mitigation: keep the
  subpackage boundaries, but inline trivial helpers such as
  `mutation_results.py` into their nearest owner.
- OCC arbiter rollback semantics differ between legacy single-edit callers and multi-edit (`edit_pipeline`) paths. Mitigation: the conflict test above is the gate.
- `OCCClient` becomes a second public API. Mitigation: importer allowlist
  permits it only from `sandbox.api.write`, `sandbox.api.edit`, runtime tests,
  and temporary migration shims; agent tools still import only `sandbox.api.*`.
