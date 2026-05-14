# OCC Architecture Remediation Implementation Report

Source review: `.planning/code-reviews/occ-architecture-review.md`

## Phase 1 — Change Dispatch And Staging Contracts

Status: complete

Changes:

- Added `sandbox.occ.merge.policy.MergePolicy` plus shared staging callable
  aliases.
- Reworked `WriteChange` around eager and disk-backed payload objects while
  preserving existing constructor call sites.
- Added cached reads for disk-backed write payloads.
- Normalized `OpaqueDirChange.kept_children` so only direct child names are
  accepted.
- Replaced direct/gated `isinstance(change, ...)` cascades with handler tables.
- Wired `OccCommitTransaction` through a `RouteDecision -> MergePolicy` map.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_occ/test_changeset_builders.py backend/tests/unit_test/test_sandbox/test_occ/test_direct_merge.py backend/tests/unit_test/test_sandbox/test_occ/test_tracked_merge.py backend/tests/unit_test/test_sandbox/test_occ/test_base_hash_inference.py -q`
- `python3 -m compileall -q backend/src/sandbox/occ`
- `rg -n "isinstance\\(change" backend/src/sandbox/occ/merge backend/src/sandbox/occ/commit_transaction.py`

Result:

- 16 tests passed.
- OCC package compiled.
- No remaining `isinstance(change, ...)` usage in the stagers or commit
  transaction.

## Phase 2 — Routing And Hashing Consolidation

Status: complete

Changes:

- Added canonical route names: `RouteDecision.GATED` and
  `RouteDecision.DIRECT`.
- Reworked `OccOrchestrator` into a canonical `Router` implementation while
  keeping the old name as a compatibility alias.
- Folded single-path preparation into `Router.prepare_single_path_sync`; the
  `routing/single_path.py` module is now a thin compatibility wrapper.
- Deleted `routing/runtime_ops.py` and moved hash helpers into
  `sandbox.occ.content.hashing`.
- Added explicit `SnapshotGitignoreMatcher` and `GitignoreCacheStats`
  protocols.
- Replaced snapshot gitignore `getattr` probing with a fail-closed protocol
  check.
- Updated unit-test gitignore fakes that route against snapshots to implement
  `is_ignored_in_snapshot`.

Verification:

- `uv run pytest backend/tests/unit_test/test_sandbox/test_occ/test_changeset_routing.py backend/tests/unit_test/test_sandbox/test_occ/test_base_hash_inference.py backend/tests/unit_test/test_sandbox/test_occ/test_commit_transaction.py backend/tests/unit_test/test_sandbox/test_occ/test_gitignore_policy_edge_cases.py -q`
- `python3 -m compileall -q backend/src/sandbox/occ`
- `test ! -e backend/src/sandbox/occ/routing/runtime_ops.py`
- `rg -n "getattr\\(oracle|is_ignored_in_snapshot" backend/src/sandbox/occ/routing backend/src/sandbox/occ/content/gitignore_oracle.py`

Result:

- 18 tests passed.
- OCC package compiled.
- `runtime_ops.py` is removed.
- Routing no longer probes `is_ignored_in_snapshot` with `getattr`; snapshot
  routing requires the explicit snapshot-aware protocol.
