# live_e2e_test — sandbox runtime live suite

Implementation of `.omc/plans/per-call-snapshot-layer-stack-migration/live-e2e-test-suite-plan.md`.

## Status

This package is opt-in and keeps only tests that execute against a real Daytona
sandbox. The old live `occ/` suite, the local layer-stack slice, and the overlay
upper-capture round-trip were removed because they exercised local Python state
in the pytest process instead of the sandbox runtime guardrails.

- **Overlay syscall slice:** `overlay/` keeps direct in-sandbox `mount(2)`,
  latency, read, concurrent-mount, and heavy-write probes via `raw_exec`.
- **Integrated slice:** `layer_stack_overlay_occ/` now has an active public-tool
  smoke for sandbox-local write/edit/shell/read coverage. Heavier race,
  recovery, and load-profile cases remain pending.

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

Current full sandbox-only live run (2026-05-05): `11 passed, 17 skipped` in
about 48 s. The skips are pending integrated concurrency/load/failure tests.

## What's Left

| Bucket | Files | Tests | Blocker |
|---|---|---:|---|
| `layer_stack_overlay_occ/*` | 5 | 17 | concurrency/load/failure helpers over public sandbox runtime tools |

The integrated suite is the replacement target for OCC/overlay live coverage.
Per the import fence in `conftest.py`, live-suite files may import public
sandbox APIs and harness helpers, but direct imports of `sandbox.layer_stack`,
`sandbox.overlay`, or `sandbox.occ` are a collection error.

## How To Run

The suite is opt-in by directory:

```bash
.venv/bin/pytest backend/tests/live_e2e_test
.venv/bin/pytest backend/tests/live_e2e_test/overlay
.venv/bin/pytest backend/tests/live_e2e_test/layer_stack_overlay_occ
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
