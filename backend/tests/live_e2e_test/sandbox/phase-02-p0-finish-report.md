# Phase 2 P0 Finish Implementation Report

Date: 2026-05-05

## Scope

Implemented Phase 2 of `backend/tests/live_e2e_test/sandbox/IMPLEMENTATION_PLAN.md`.

New live native probe files:

- `layer_stack/test_squash.py`
- `layer_stack/test_changes_aggregation.py`
- `layer_stack/test_lease_registry.py`
- `layer_stack/test_lease_budget.py`
- `layer_stack/test_stack_manager_integration.py`

Runtime helpers added to support the Phase 2 contract:

- `sandbox.layer_stack.changes.aggregate_layer_changes`
- `LeaseRegistry.expire_older_than`
- `LeaseRegistry.sweep_dead_owners`
- `LayerStackManager.expire_leases_older_than`
- `LayerStackManager.sweep_dead_lease_owners`
- `LeaseBudgetWorker(max_active_depth=0)` now represents a closed budget.

## Verification

Environment:

```bash
EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1
```

Commands run:

```bash
uv run python -m py_compile \
  backend/src/sandbox/layer_stack/changes.py \
  backend/src/sandbox/layer_stack/lease_registry.py \
  backend/src/sandbox/layer_stack/lease_budget.py \
  backend/src/sandbox/layer_stack/stack_manager.py \
  backend/tests/live_e2e_test/sandbox/layer_stack/test_squash.py \
  backend/tests/live_e2e_test/sandbox/layer_stack/test_changes_aggregation.py \
  backend/tests/live_e2e_test/sandbox/layer_stack/test_lease_registry.py \
  backend/tests/live_e2e_test/sandbox/layer_stack/test_lease_budget.py \
  backend/tests/live_e2e_test/sandbox/layer_stack/test_stack_manager_integration.py

.venv/bin/pytest --collect-only backend/tests/live_e2e_test/sandbox/layer_stack -q
uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack -q
uv run ruff check backend/src/sandbox/layer_stack backend/tests/live_e2e_test/sandbox/layer_stack

EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1 \
  .venv/bin/pytest \
  backend/tests/live_e2e_test/sandbox/layer_stack \
  backend/tests/live_e2e_test/sandbox/occ \
  backend/tests/live_e2e_test/sandbox/overlay \
  -v -s --tb=short
```

Results:

| Check | Result |
|---|---:|
| py_compile | passed |
| live layer_stack collect-only | 15 collected |
| unit layer_stack suite | 25 passed |
| ruff targeted check | passed |
| Phase 2 live gate | 43 passed, 1 warning, 102.52 s |

The historical API load command from older reports,
`backend/tests/test_sandbox/test_api/test_load.py`, is not present in this
checkout. The load metrics below therefore come from the current live gate's
load-shaped and race probes, not the future Phase 4 integrated load profiles.
`layer_stack_overlay_occ/test_load_profiles.py` still contains skip stubs.

## Phase 2 Metrics

All new native probes emitted resource blocks. For the new Phase 2 probes,
`fd_open` stayed at 16 and mount counts stayed at `mounts=13`,
`overlay_mounts=3`; no fd, mount, or overlay-mount leaks were observed.

| Probe | Workload | Duration | Key result |
|---|---:|---:|---|
| changes aggregation | 6 input changes | 24.4 ms | deduped to 4 paths; rename pair preserved |
| changes aggregation race | 8 producers, 16 changes | 38.6 ms | deduped to 8 final paths; deterministic path order |
| lease budget | zero/one/infinite budgets | 24.4 ms | budget 0 rejected, budget 1 allowed then rejected, infinite allowed |
| lease budget race | 8 concurrent publishes, budget 4 | 39.8 ms | exactly 4 accepted, 4 rejected |
| lease registry | release/expire/sweep | 21.9 ms | double release was a no-op; stale owner swept |
| lease registry race | 16 concurrent leases | 38.0 ms | 16 unique ids; 16 released; final refcount 0 |
| squash | 6 layers to depth 2 | 30.3 ms | 5 layers coalesced; stale staging removed |
| squash race | squash plus concurrent append | 36.0 ms | final depth 3; lost appends 0 |
| stack manager | publish/read/materialize/failures/gc | 29.7 ms | bad hash and backpressure rejected; fsck clean |
| stack manager race | 4 concurrent agents | 36.1 ms | manifest depth 5; all leases released |

Race latency highlights:

| Probe | p50 | p99 | Max |
|---|---:|---:|---:|
| manifest append race, N=8 | 4.94 ms | 6.79 ms | 6.82 ms |
| publisher same-digest race, N=8 | 2.62 ms | 3.03 ms | 3.04 ms |
| stack manager agent race, N=4 | 3.21 ms | 4.47 ms | 4.51 ms |

## P0 Native Gate Metrics

Layer-stack foundation metrics:

| Probe | Metric | Result |
|---|---|---:|
| merged view | depth resolved | 103 layers |
| merged view | duration | 147.7 ms |
| manifest lifecycle | restart + corruption detection | passed |
| publisher | idempotent same digest | passed |

OCC metrics:

| Probe | Workload | Result |
|---|---|---:|
| commit transaction race | 4 concurrent commits | 1 accepted, 3 aborted |
| commit transaction race | p99 | 7.80 ms |
| direct route large changeset | 10,000 paths | 1,304.5 ms |
| direct route large changeset | accepted files | 10,000 |
| direct route race | 8 disjoint commits | 8 accepted, p99 10.04 ms |
| serial merger race | 16 waiters | FIFO upheld, p99 wait 43.63 ms |
| serial merger race | max wait | 44.16 ms |
| orchestrator race | 4 concurrent writers | 1 accepted, 3 aborted |
| gitignore oracle | nested/reinclude/case variants | passed |
| merge engine | conflict/binary/CRLF/direct | passed |

Overlay native metrics:

| Probe | Workload | Result |
|---|---|---:|
| capture changes | whiteout/opaque/rename/order | 20.3 ms |
| capture changes race | 4 writers same path | 1 final path, 28.9 ms |
| capture upperdir | binary/sparse/symlink/hardlink/long/unicode | 27.6 ms |
| snapshot runner | shell capture | 328.2 ms |
| snapshot runner race | 4 parallel runners | p99 296.26 ms |

Overlay syscall load-shaped metrics:

| Probe | Workload | Result |
|---|---|---:|
| concurrent mounts | 200 simultaneous mounts at depth 50 | 0 failures, mount p99 0.474 ms |
| heavy write copy-up | 1,000 files, 256 B each | 83,363 writes/s, p99 0.0418 ms |
| mount depth | depth 200 direct syscall | 0.222 ms |
| snapshot latency | depth 100, 1,000 iterations | p99 0.453 ms |
| snapshot latency | depth 200, 1,000 iterations | p99 0.546 ms |
| zero-failure probe | depth 100, 1,000 iterations | 0 failures, p99 0.357 ms |
| zero-failure probe | depth 200, 1,000 iterations | 0 failures, p99 0.519 ms |
| warm read | depth 100 vs depth 1 | 1.016x |
| cold read | depth 50 vs depth 1 | 1.977x |

## Complex And Edge Case Handling

Covered by the Phase 2 implementation:

- Squash coalesces old suffix layers into a checkpoint, keeps the merged view
  correct, is idempotent below the target depth, and recovers a stale
  mid-squash staging directory through fsck cleanup.
- Squash racing with an append preserves every pre-existing file and the
  concurrent append; no torn manifest or lost append was observed.
- Change aggregation collapses duplicate same-path writes, preserves delete +
  write rename pairs, and produces deterministic per-path ordering under
  concurrent producers.
- Lease registry supports register, release, double-release no-op, age expiry,
  dead-owner sweep, exact refcounts, and unique concurrent lease ids.
- Lease budget handles closed, single-slot, and infinite depth budgets, refreshes
  after lease release, and enforces the boundary exactly under concurrency.
- Stack manager integration covers publish, snapshot lease reads, active reads,
  materialization, hash-mismatch failure, backpressure failure, lease expiry,
  squash, GC, and concurrent agent publishing.

Residual gaps:

- The Phase 4 integrated load profiles remain skip stubs and were not claimed as
  passing here.
- The old local API load suite referenced by earlier reports is absent from this
  checkout, so no current `read/write/edit/shell` API load numbers were produced
  in this Phase 2 pass.
