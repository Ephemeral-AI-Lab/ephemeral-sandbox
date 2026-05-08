# live_e2e_test — sandbox runtime live suite

Implementation of `.omc/plans/per-call-snapshot-layer-stack-migration/live-e2e-test-suite-plan.md`.

## Status

This package is opt-in and keeps only tests that execute against a real Daytona
sandbox. The old live `occ/` suite, the local layer-stack slice, and the overlay
upper-capture round-trip were removed because they exercised local Python state
in the pytest process instead of the sandbox runtime guardrails.

- **Overlay syscall slice:** `sandbox/overlay/syscall/` keeps direct in-sandbox `mount(2)`,
  latency, read, concurrent-mount, and heavy-write probes via `raw_exec`.
- **Overlay native slice:** `sandbox/overlay/native/` (in progress) hosts probes
  that import `sandbox.overlay` from the runtime bundle inside the sandbox.
- **Phase 1 native foundations:** `sandbox/layer_stack/`, `sandbox/occ/`, and
  `sandbox/overlay/native/` now cover the P0 storage, OCC, and native capture
  probes from `IMPLEMENTATION_PLAN.md`. Current live gate: `23 passed` in
  `55.65 s` on 2026-05-05. See `phase-01-native-foundations-report.md`.
- **Phase 3 integrated P0:** `sandbox/layer_stack_overlay_occ/` now covers the
  public-tool write/edit/read/shell path, shell snapshot isolation, concurrent
  shell+edit agents, tracked vs gitignored codegen races, and recovery cleanup.
  Last full live gate: `14 passed, 4 skipped` in `151.73 s` on 2026-05-05. See
  `phase-03-integrated-p0-report.md`.
- **Phase 4 P1 load/resource/edge:** `overlay/native/`, `layer_stack/`,
  `occ/`, and `layer_stack_overlay_occ/test_load_profiles.py` now cover the P1
  edge cases, resource probes, native subsystem load, and integrated
  smoke/sustained/burst profiles. Current full Phase 4 live gate: `17 passed,
  1 deselected` in `142.67 s` on 2026-05-05 UTC. Current focused pure public
  API load gate, after shell batching: `3 passed, 1 deselected` in `70.52 s`.
  See
  `phase-04-p1-load-resource-edge-report.md`.
- **Request snapshot Phase 0:** `sandbox/request_snapshot/` measures real
  `/testbed` request snapshot create/destroy latency for `copy_cp`, `tar_copy`,
  `reflink_cp`, and `hardlink_cp`, including 1/5/10 concurrent create batches.
  Current live gate: `3 passed` in `12.73 s` on 2026-05-06 UTC. See
  `request_snapshot/phase-00-request-snapshot-probe-report.md`.
- **Workspace base Phase 01:** `sandbox/workspace_base/` imports the real
  `/testbed` repository as layer-stack manifest version 1, then measures base
  import, 20-way build concurrency, correctness, fail-closed import hazards,
  layer creation, materialization, and squash/lease behavior. Current live
  gate: `8 passed` in `70.03 s` on 2026-05-06 UTC. Compatibility read-load
  smoke: `1 passed` in `12.29 s`; 32 public reads over 16 base paths, runtime
  max `0.741 ms`, wall max `526.834 ms`. See
  `phase-01-workspace-base-report.md`.
- **Phase 05 public file ops:** `layer_stack_overlay_occ/test_phase05_*` proves
  public `read_file`, `write_file`, `edit_file`, and `shell` over an imported
  `/testbed` base, including raw workspace mutation isolation, full-filesystem
  passthrough outside `/testbed`, symlink classification, conflict/fail-closed
  cases, nonzero shell side-effect policy, and `1/5/10/20` load for read,
  write, edit, shell, and mixed workloads. Current focused live gate:
  `7 passed` in `226.96 s` on 2026-05-07 UTC with documented budget overrides.
  Draft default c20 redlines still miss for write/edit/shell; see
  `phase-05-public-file-ops-report.md`.

Current overlay syscall run (2026-05-05, full battery, 1000 iter x 8 depths):

| Test | Result |
|---|---|
| `test_mount_depth.py` (3 tests) | mount(2) rc=0 across {1,5,10,30,50,80,100,200}; mount(8) fails at depth 100 with options_len=7772 |
| `test_snapshot_latency.py` (3 tests) | 0 failures across 8000 calls; p99 at depth 100 = 0.30 ms; depth 200 p99 = 0.43 ms |
| `test_read_latency.py` (2 tests) | warm at depth 100 = 1.05x depth-1 baseline; cold at depth 50 = 1.28x baseline |

Current integrated public-tool smoke (2026-05-05):
`1 passed` in about 10 s against a real Daytona sandbox. The test writes,
edits, shells, and reads through the public sandbox API, with OCC/overlay
running from the uploaded sandbox runtime bundle.

Last full Phase 3 integrated live run (2026-05-05): `14 passed, 4 skipped` in
`151.73 s`. The skips are Phase 4/5 load-profile tests.

Current focused public API scaling run (2026-05-05 UTC):
`test_concurrency_scaling.py` uses independent `sandbox.api.read_file`,
`write_file`, `edit_file`, and `shell` calls and passed in `142.58 s` with
1/5/10/20 concurrent calls for each verb. At concurrency 20, throughput was:
read `13.624 ops/s`, write `6.921 ops/s`, edit `6.292 ops/s`, shell
`5.089 ops/s`.

Current focused gitignored-overlap run (2026-05-05 UTC):
`test_overlapping_50pct_gitignored_paths_use_lww` now uses raw `process.exec`
for the 8 overlapping writers and passed in `9.55 s`; its per-call JSONL is
`.omc/results/live-e2e-phase3-gitignored-overlap-process-exec-20260505T162158Z.jsonl`.

Current Phase 4 P1 full run (2026-05-05 UTC):
`17 passed, 1 deselected` in `142.67 s`. Integrated JSONL artifacts:

- `.omc/results/live-e2e-integrated-smoke-20260505T165416Z.jsonl`
- `.omc/results/live-e2e-integrated-sustained-20260505T165446Z.jsonl`
- `.omc/results/live-e2e-integrated-burst-20260505T165544Z.jsonl`

Current focused pure public API load run after shell batching
(2026-05-05 UTC): `3 passed, 1 deselected` in `70.52 s`. Integrated JSONL
artifacts:

- `.omc/results/live-e2e-integrated-smoke-20260505T173106Z.jsonl`
- `.omc/results/live-e2e-integrated-sustained-20260505T173125Z.jsonl`
- `.omc/results/live-e2e-integrated-burst-20260505T173203Z.jsonl`

See `phase-04-pure-sandbox-api-load-report.md` for current pure
`sandbox.api.*` performance metrics.

## What's Left

| Bucket | Files | Tests | Blocker |
|---|---|---:|---|
| Phase 5 soak | 1 | 1 | 15-minute soak run and leak regression gate |
| P2 stress/nightly profiles | 3 | 3 | explicit nightly-only stress window |

The integrated suite is the replacement target for OCC/overlay live coverage.
Per the import fence in `conftest.py`, live-suite files may import public
sandbox APIs and harness helpers, but direct imports of `sandbox.layer_stack`,
`sandbox.overlay`, or `sandbox.occ` are a collection error.

## How To Run

The suite is opt-in by directory:

```bash
.venv/bin/pytest backend/tests/live_e2e_test
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/overlay
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/request_snapshot -q -s
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/workspace_base -v -rs -s --tb=short
.venv/bin/pytest \
  backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ/test_phase05_public_file_ops_correctness.py \
  backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ/test_phase05_full_filesystem_view.py \
  backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ/test_phase05_public_file_ops_edge_cases.py \
  backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ/test_phase05_public_file_ops_load.py \
  -v -rs -s --tb=short
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ
.venv/bin/pytest backend/tests/live_e2e_test -v -rs
```

All tests carry the `live` marker, so `pytest -m "not live"` excludes them.

## Prerequisites

The session fixture brings up a real Daytona sandbox via `setup_after_create`.
Before running, configure Daytona credentials and set a prebaked sandbox image
with Python 3.10+, `git`, `/testbed`, and the runtime bundle marker:

```bash
EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1
```

The session-scoped `live_sandbox` fixture starts one Daytona sandbox per run
and resets `/testbed` before each test with `git reset --hard HEAD` plus
`git clean -fdx`.

## Defaults

| Plan question | Adopted default |
|---|---|
| sandbox lifecycle scope | session-scoped sandbox + per-test `/testbed` reset |
| load JSONL location | `.omc/results/live-e2e-<profile>-<utc>.jsonl` |
| import fence enforcement | `pytest_collection_modifyitems` hook in `conftest.py` |
| drift definition under load | realtime check and post-run replay reconciliation |
| burst emergency-depth budget | 0 |
| provider neutrality | facade-first; only `live_sandbox` mentions Daytona |

See `load_testing_standard.md` for pass bars and load profiles.
