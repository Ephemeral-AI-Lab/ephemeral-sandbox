# OCC Architecture Remediation Plan

Source review: `.planning/code-reviews/occ-architecture-review.md`

## Phase 1 — Change Dispatch And Staging Contracts

Issues covered:

- Critical `Change` kind `isinstance` cascades in direct and gated stagers.
- Missing shared `MergePolicy` contract.
- Frozen dataclasses with hand-rolled copy/constructor behavior.
- `WriteChange` transport-vs-intent leak and repeated lazy disk reads.
- `_FinalKind` only existing in direct staging.
- `OpaqueDirChange` accepts unvalidated child names.

Implementation:

- Keep the caller-facing change constructors stable.
- Split write content into eager and disk-backed payload objects.
- Replace direct/gated change-kind cascades with typed handler tables.
- Add a shared staging policy protocol and use a route-to-policy map in the
  commit transaction.
- Normalize opaque-dir kept children at construction.

Verification:

- `test_changeset_builders.py`
- `test_direct_merge.py`
- `test_tracked_merge.py`
- `test_base_hash_inference.py`

## Phase 2 — Routing And Hashing Consolidation

Issues covered:

- Duplicate batch router and single-path router.
- `GitignoreMatcher` vs `SnapshotIgnoreOracle` protocol split.
- Runtime `getattr` probing for snapshot-aware gitignore behavior.
- `routing/runtime_ops.py` junk-drawer hash helper file.
- Double-negative `RouteDecision.OCC_SKIPPED_MERGE` naming.

Implementation:

- Move routing into one router with a single-path fast branch.
- Make snapshot-aware gitignore support an explicit protocol requirement when
  a snapshot is provided.
- Move runtime hash helpers into content hashing.
- Introduce canonical `RouteDecision.DIRECT` and `RouteDecision.GATED` names,
  with temporary aliases only where compatibility is needed.

## Phase 3 — Service, Ports, Maintenance, And Queue Lifecycle

Issues covered:

- `OccLayerStackPorts` welded interface.
- Auto-squash logic living inside service and probing `squash` via `getattr`.
- `OccService` constructing all collaborators internally.
- CAS retry budget as a module constant instead of retry policy.
- Serial merger thread starts in `__init__` and has no shutdown.
- Hardcoded timing keys without a registry.
- `_LayerChangeStager` is private despite being a natural backend seam.

Implementation:

- Make service accept explicit snapshot/staging/publisher ports and injectable
  router, transaction, queue, and maintenance policy.
- Extract auto-squash into a maintenance policy.
- Add queue `start()` / `close()` lifecycle and a retry policy value object.
- Promote the layer-change stager contract and add timing key constants for
  touched OCC timing surfaces.

## Phase 4 — Naming, Structure, And Consumer Ownership

Issues covered:

- Inconsistent `Occ` / `OCC` prefixes.
- Misleading class names: orchestrator, merger, merge.
- Top-level `commit_transaction.py` placement.
- `result_projection.py` living in the OCC engine package.
- Empty/asymmetric package `__init__.py` files.
- Long import paths for overlay capture and routing helpers.
- Low-impact naming nits in overlay kept-child helpers and serial timing keys.

Implementation:

- Canonical names: `Service`, `Client`, `Router`, `CommitTransaction`,
  `CommitQueue`, `DirectStager`, `GatedStager`.
- Move staging files under a `stage` package and move queue/route/overlay/result
  helper ownership to their actual consumers.
- Update runtime, tests, bundle assertions, and docs to use canonical paths.
