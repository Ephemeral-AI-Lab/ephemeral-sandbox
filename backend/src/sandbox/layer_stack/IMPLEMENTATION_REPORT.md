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
- `make_layer_change(...)` remains as an explicit factory for code that genuinely has a parsed kind string.

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
