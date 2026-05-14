# layer_stack Remediation Implementation Report

Source review: `backend/src/sandbox/layer_stack/REVIEW.md`

## Remediation Plan

The review items are handled in small, testable phases:

1. Layer-change contract and publish path
   - Replace the `LayerChange.__new__` string factory with explicit variant construction.
   - Move digest and write behavior onto the change variants.
   - Keep `LayerDelta`/`aggregate_layer_changes` as public OCC-facing contracts and make publisher dedup use the same helper.

2. Manifest, errors, and public facade
   - Add manifest `schema_version` with a forward-compatibility guard.
   - Move storage-domain errors out of the merged-view implementation.
   - Expand the root `sandbox.layer_stack` facade so consumers do not need deep imports for common symbols.
   - Stop exporting overlay marker constants from the merged-view module; keep `layer.index` as the canonical source.

3. Manager, transaction, and storage seams
   - Move `LayerStackTransaction` out of `manager.py`.
   - Add narrow protocols for manifest/view/publisher/lease collaborators and constructor injection seams.
   - Rename the publisher lock contract away from `publish_layer_locked`.
   - Move path helpers to a private module and fix the process lock lifecycle.

4. Naming, layout, and compatibility cleanup
   - Rename ambiguous helpers and public keywords where compatibility allows.
   - Remove the workspace-base layer ID collision by moving the base prefix out of runtime layer ID space.
   - Split dual-purpose workspace path translation into explicit relative and absolute methods.
   - Make package `__init__.py` files expose deliberate facades or keep them empty.

5. Verification
   - Run focused unit tests after each phase.
   - Finish with the layer-stack unit suite and the nearest OCC/overlay tests touched by imports or contracts.

## Phase Log

### Phase 1: Layer-change contract and publish path

Status: complete.

Review issues addressed:

- C-1: `LayerChange` is now an abstract contract instead of a `__new__` string-dispatch factory.
- M-10: `LayerDelta`/`aggregate_layer_changes` remain public because OCC uses them, and publisher preparation now uses the same aggregation helper before digest/write work.
- C-1 follow-up: digest contribution and layer writes moved onto explicit change variants, so publisher no longer dispatches per kind with `isinstance`.

Implementation notes:

- Production OCC code now constructs `WriteLayerChange`, `DeleteLayerChange`, `SymlinkLayerChange`, or `OpaqueDirLayerChange` directly.
- The temporary `make_layer_change(...)` compatibility factory was removed after the cleanup pass found no in-repo callers.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack -q` -> `68 passed`.

### Phase 2: Manifest, errors, and public facade

Status: complete.

Review issues addressed:

- C-4: manifest JSON now includes `schema_version`, accepts legacy v1 payloads without the key, and rejects newer schema versions explicitly.
- H-4: overlay marker constants are no longer exported from `view.merged`; consumers use `layer.index`.
- H-5: `LayerStackStorageError` moved to `sandbox.layer_stack.errors` and is exported from the root facade.
- M-9: the root facade now exports layer-change variants, aggregation helpers, manifest schema version, and storage errors.

Implementation notes:

- `ManifestConflictError` now lives with other layer-stack domain errors while remaining import-compatible through `sandbox.layer_stack.manifest`.
- Live probe snippets and unit tests now import `OPAQUE_MARKER` / `WHITEOUT_PREFIX` from `sandbox.layer_stack.layer.index`.
- Verification initially exposed a concurrent checkout mismatch where `occ.routing.runtime_ops` had been deleted while `occ.service` still referenced it; the current `occ.service` import now points at `sandbox.occ.content.hashing.infer_manifest_base_hash`.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack backend/tests/unit_test/test_sandbox/test_overlay/test_upperdir_capture.py -q` -> `75 passed`.

### Phase 3: Manager, transaction, and storage seams

Status: complete.

Review issues addressed:

- C-2/H-2: `LayerStackTransaction` moved out of `manager.py` into `transaction.py` with an explicit `LayerStackTransactionHandle`.
- C-3: added `ManifestStore`, `SnapshotMaterializer`, `ChangePublisher`, `LeaseStore`, and `CommitStagingStore` protocols; `LayerStackManager` accepts injected collaborators.
- H-3/M-4: filesystem helpers moved to private `_paths.py`, with request-safe naming and cleanup logging outside `manager.py`.
- M-3: publisher method renamed from `publish_layer_locked` to `publish_layer`; the lock contract is now held at the transaction boundary.
- M-7: materialization keyword renamed from `link_ok` to `share_inodes`.
- L-3: unreferenced-layer cleanup now preserves candidate order while deduplicating instead of sorting by `layer_id`.
- L-4: publisher layer-ID allocation now uses the shared path allocator.
- L-7: storage writer locks now use refcounted leases with `close()`/`__del__` cleanup instead of an immortal fd cache.

Implementation notes:

- `FileManifestStore` keeps the existing on-disk manifest behavior behind the new manifest-store protocol.
- A current OCC checkout mismatch also blocked this phase: `_default_maintenance` had been removed while `OccService` still called it. The helper was restored as a thin selector between `AutoSquashMaintenancePolicy` and `NoopMaintenancePolicy`.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack backend/tests/unit_test/test_sandbox/test_occ/test_mutation_gate.py -q` -> `75 passed`.

### Phase 4: Naming, layout, and compatibility cleanup

Status: complete.

Review issues addressed:

- C-5: the unused `snapshot_dir` alias was removed in the cleanup pass; `lowerdir` remains because it is the active command-exec/overlay mount contract.
- C-6: workspace-base layers moved out of the runtime `L...` namespace; the base sentinel is now `B000001-base`.
- H-1/L-1: package `__init__.py` files now act as deliberate facades for layer, commit, lease, maintenance, view, and workspace APIs.
- M-1: the squash collaborator is now `SquashService`; the old `SquashWorker` compatibility alias was removed.
- M-2: vague `commit/staging.py` was renamed to `commit/commit_staging_area.py`.
- M-5: duplicate publish timing was removed; publish preparation now reports one timing key.
- M-6: the public `opaque_dir` discriminator remains for overlay/runtime compatibility; the explicit `OpaqueDirLayerChange` variant now isolates the odd name behind a typed constructor instead of the old string factory.
- M-8: manifest internals now use private `_model.py`; the `manifest` package facade is the canonical public import path.
- M-11: common layer-stack symbols are exported through root/package facades, and the current sandbox boundary/bundle tests cover the intended public surface.
- L-2: `WorkspaceBaseIncompleteError` no longer inherits from workspace-binding errors.
- L-4: the shared unique-layer allocator is used by both layer publish and squash.
- L-6: workspace binding now has explicit `layer_path_from_relative(...)` and `layer_path_from_absolute(...)` methods; the old dual-semantics wrapper was removed.
- L-8: the manager now has protocol-backed collaborator seams. A complete memory-only backend was not added because the production contract remains filesystem-backed, but the test seam no longer requires reaching through concrete storage classes.

Implementation notes:

- The physical package layout was not flattened further because existing import paths are part of the runtime/test surface. The facades now provide the intended shallow imports without breaking deeper compatibility paths.
- Final verification exposed unrelated concurrent sandbox/OCC rename drift in the current checkout. Compatibility aliases and bundle/dependency expectations were aligned with the current `occ.router`, `occ.stage`, and runtime-bundle layout so the sandbox slices import and execute.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack backend/tests/unit_test/test_sandbox/test_occ/test_mutation_gate.py backend/tests/unit_test/test_sandbox/test_overlay/test_upperdir_capture.py -q` -> `81 passed`.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_occ/test_auto_squash.py -q` -> `5 passed`.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/test_runtime_ready.py -q` -> `7 passed`.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_command_exec backend/tests/unit_test/test_sandbox/test_overlay/test_upperdir_capture.py backend/tests/unit_test/test_sandbox/test_overlay/test_overlay_dependency_boundaries.py backend/tests/unit_test/test_sandbox/test_daemon/test_runtime_ready.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py backend/tests/unit_test/test_sandbox/test_import_fence.py -q` -> `231 passed`.

### Phase 5: OCC legacy-name cleanup

Status: complete.

Review issues addressed:

- Removed the legacy generic OCC service/client symbols from the runtime surface.
- Standardized in-repo callers on `OccService(gitignore=..., layer_stack=...)` and `OCCClient`.
- Removed the old explicit `snapshot_reader`/`staging`/`publisher` constructor path from `OccService`; the layer stack is the canonical combined OCC port.

Implementation notes:

- `OccService` now owns the default auto-squash maintenance policy selection directly from the layer-stack capability.
- Runtime backend construction, OCC unit tests, live OCC probes, and architecture notes were aligned to the canonical names.

Verification:

- `python3 -m py_compile backend/src/sandbox/occ/service.py backend/src/sandbox/occ/client.py backend/src/sandbox/runtime/daemon/service/occ_backend.py` -> passed.
- `uv run ruff check backend/src/sandbox/occ backend/src/sandbox/runtime/daemon/service/occ_backend.py backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_daemon/test_daemon.py backend/tests/live_e2e_test/sandbox/occ --select F401,F811,F821,F841,UP035 -q` -> passed.
- `uv run pytest backend/tests/unit_test/test_sandbox/test_occ backend/tests/unit_test/test_sandbox/test_daemon/test_daemon.py -q` -> `67 passed`.
