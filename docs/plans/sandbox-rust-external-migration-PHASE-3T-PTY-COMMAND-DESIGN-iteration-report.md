# Phase 3T PTY Command Iteration Report

## Iteration 1 - 2026-06-01 11:39 CST

- Checkout: `565f4ea22` with the Phase 3T implementation changes in the worktree.
- Plan path: `docs/plans/sandbox-rust-external-migration-PHASE-3T-PTY-COMMAND-DESIGN.md`.
- Scope: Docker-only live E2E, load, and p95 performance gates. Daytona is intentionally skipped.
- Current implementation evidence before live run:
  - `cargo test -p eos-protocol -p eos-runner -p eos-daemon`: passed.
  - `cargo check -p eos-daemon --target x86_64-unknown-linux-musl`: passed.
  - focused Python pytest/ruff/tool-registry checks: passed.
- Setup evidence:
  - Docker CLI is available via `/usr/local/bin/docker`.
  - Docker server is Linux/arm64 through Docker Desktop.
  - Local images include `sweevo-dask__dask-10042:latest` and `xingyaoww/sweb.eval.x86_64.dask_s_dask-10042:latest`.
  - Local artifacts exist at `sandbox/dist/eosd-linux-amd64` and `sandbox/dist/eosd-linux-arm64`.
- First gap found:
  - `backend/scripts/bench_rust_daemon_phase3.py` is useful for Docker setup, artifact upload, LayerStack seeding, daemon startup, and load/report patterns, but it measures the older `api.v1.shell` raw-argv CP-4 surface. Phase 3T closeout requires fresh `api.v1.exec_command` and PTY-control measurements.
- Next entry point:
  - Run Docker artifact verification, then run or add a Phase 3T-specific Docker benchmark path for finite command, PTY start/progress/write/cancel, load matrix, and p95 gates.

## Iteration 2 - 2026-06-01 11:52 CST

### Runtime Artifact

- Built and uploaded fresh Linux amd64 `eosd` for Docker live checks.
- Report: `bench/local-eosd-amd64-phase3t-20260601.json`.
- Artifact: `sandbox/dist/eosd-linux-amd64`.
- SHA-256:
  `0f540967c790787e0076c6cbbf624c54c05a66f026e1e8ba0fca1fdca70972d5`.
- Upload gate: passed; remote mode `0o755`; `eosd --version` returned
  `eosd 0.1.0`.

### Full Tiered Docker Live E2E

- Command family:
  `backend.tests.live_e2e_test._tools.run_tiered --provider docker --tier 0,1,2,3,4,5,6 --run-id phase3t-docker-20260601-rust-control-op-blocker`.
- Environment:
  `EOS_SANDBOX_PROVIDER=docker`, `EOS_SANDBOX_RUNTIME=rust`,
  `EOS_LIVE_E2E_IMAGE=sweevo-dask__dask-10042:latest`,
  `EOS_DOCKER_PRIVILEGED=1`.
- Latest summary artifact:
  `.omc/results/progressive-test-summary-phase3t-docker-20260601-rust-control-op-blocker.jsonl`.
- Tier result:
  - Tier 0 preflight: passed in 0.711 s.
  - Tier 1 smoke: failed in 8.260 s.
  - Tiers 2-6: skipped by cascade.
- Direct Tier 1 smoke rerun after the Rust runtime upload fix reached the Rust
  daemon and failed on:
  `unknown_op: unknown op: api.ensure_workspace_base`.
- Verdict: full tiered Docker live E2E is blocked by a Rust daemon control-plane
  coverage gap, not by the Phase 3T PTY command surface. The live suite requires
  layer-stack workspace-base setup before it can exercise the later tiers under
  `EOS_SANDBOX_RUNTIME=rust`.

### Phase 3T Docker PTY/Load/P95 Gate

- Added dedicated harness:
  `backend/scripts/bench_rust_daemon_phase3t_pty.py`.
- Strict report:
  `bench/phase3t-pty-command-docker-20260601-strict.json`.
- Top-level gate: passed.
- Correctness checks: passed.
  - stdout/stderr split.
  - command environment resolves `python` to
    `/opt/miniconda3/envs/testbed/bin/python`.
  - finite command writes publish through OCC and are readable.
  - finite `nohup ... &` descendant cleanup leaves no matching process.
- P95 gates:
  - finite `exec_command(tty=false, cmd=true)`: 49.733 ms, gate <= 60 ms.
  - `exec_command(tty=true, cmd=true)`: 49.633 ms, gate <= 100 ms.
  - `check_pty_command_progress`: 1.273 ms, gate <= 20 ms.
  - `write_pty_command_stdin` to visible echo: 57.289 ms, gate <= 100 ms.
  - `cancel_pty_command`: 55.426 ms, gate <= 500 ms.
  - cancel cleanup: 414.419 ms, gate <= 2500 ms.
- Load matrix: passed at 1/3/5/10 concurrency for finite no-op, finite write,
  and PTY no-op operations. Max observed p95 among the load cells was 167.020 ms
  for 10-way finite writes; all samples succeeded.

### Notes

- Daytona was not run.
- The first Phase 3T benchmark report,
  `bench/phase3t-pty-command-docker-20260601.json`, exposed that a
  150 ms post-write yield made the 100 ms write gate impossible.
- The second report,
  `bench/phase3t-pty-command-docker-20260601-rerun.json`, exposed a stricter
  harness issue: `pty_write_echo` p95 was under the target, but sample-level
  echo correctness was not included in the top-level gate.
- The strict harness now counts operation sample correctness in
  `operation_samples_ok` and measures PTY write latency separately from the
  follow-up progress poll that proves the child consumed stdin.

### Remaining Blocker

To make the full tiered Docker live E2E pass under `EOS_SANDBOX_RUNTIME=rust`,
the Rust daemon needs the layer-stack workspace setup control op first observed
as missing:

```text
api.ensure_workspace_base
```

The Python daemon implements this live-suite setup path today. Porting it is
outside the PTY command gate itself, but it is required before tiers 1-6 can be
used as full Rust-runtime live evidence.

### Focused Verification After Report Update

- `.venv/bin/python -m ruff check backend/scripts/bench_rust_daemon_phase3t_pty.py backend/src/sandbox/host/runtime_bundle.py backend/src/sandbox/host/runtime_artifact/__init__.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py`:
  passed.
- `.venv/bin/python -m pytest backend/tests/unit_test/test_sandbox/test_daemon/test_bundle.py backend/tests/unit_test/test_sandbox/test_daemon/test_bundle_upload.py -q`:
  16 passed.
- `git diff --check`: passed.
- `cargo check -p eos-daemon --target x86_64-unknown-linux-musl`: passed with
  pre-existing warnings in adjacent crates.

## Iteration 3 - 2026-06-01 14:15 CST

### Implementation Delta

- Implemented Rust workspace-base control ops:
  `api.ensure_workspace_base`, `api.build_workspace_base`, and
  `api.workspace_binding`.
- Moved Docker live LayerStack storage to the existing provider scratch tmpfs:
  `/eos-mount-scratch/eos-sandbox-runtime/layer-stack`.
- Fixed Rust and Python delete-layer whiteouts to prefer kernel overlay
  whiteout device nodes and retain xattr/logical fallbacks. This resolved the
  Python-vs-Rust mismatch where lower xattr whiteouts hid lookup but leaked
  placeholder names through `readdir`/`os.walk`.
- Preserved symlinks in Rust and Python workspace-base imports.
- Restored Rust OCC route/timing parity:
  `occ.commit.gated_path_count`, `occ.commit.direct_path_count`, and
  transaction-scoped `occ.commit.total_s` now come from the commit path, not the
  outer queue wait.
- Optimized gated validation by reading the active manifest once per transaction
  and caching fresh parent-directory absence for new-file workloads.
- Fixed PTY natural-exit cleanup so `tty=true` commands that spawn
  `nohup ... 2>&1 &` descendants do not leave the descendant alive after the
  PTY runner exits.

### Runtime Artifact

- Final Docker-tested amd64 artifact: `sandbox/dist/eosd-linux-amd64`.
- SHA-256:
  `71f6533c2d41861303cc7fef4828738cd16e352c539b59c67e489987f1a36162`.
- Upload report:
  `bench/local-eosd-amd64-phase3t-pty-cleanup-conditional-20260601.json`.
- Upload gate: passed; remote mode `0o755`; `eosd --version` returned
  `eosd 0.1.0`.

### Full Tiered Docker Live E2E

- Command family:
  `backend.tests.live_e2e_test._tools.run_tiered --provider docker --tier 0,1,2,3,4,5,6 --run-id phase3t-rust-scratch-full-final-20260601`.
- Environment:
  `EOS_SANDBOX_PROVIDER=docker`, `EOS_SANDBOX_RUNTIME=rust`,
  `EOS_LIVE_E2E_IMAGE=sweevo-dask__dask-10042:latest`,
  `EOS_DOCKER_PRIVILEGED=1`.
- Summary artifact:
  `.omc/results/progressive-test-summary-phase3t-rust-scratch-full-final-20260601.jsonl`.
- Tier result:
  - Tier 0 preflight: passed in 0.73 s.
  - Tier 1 smoke: passed in 12.70 s.
  - Tier 2 k-scaling spot check: passed in 12.86 s.
  - Tier 3 single-axis matrices: passed in 33.72 s.
  - Tier 4 cross-axis matrices: passed in 48.76 s.
  - Tier 5 soak: passed in 23.04 s.
  - Tier 6 adversarial: passed in 13.49 s.

### Phase 3T Docker PTY/Load/P95 Gate

- Report:
  `bench/phase3t-pty-command-docker-20260601-pty-cleanup-conditional.json`.
- Top-level gate: passed.
- Correctness checks: passed.
  - stdout/stderr split.
  - command environment resolves `python` to
    `/opt/miniconda3/envs/testbed/bin/python`.
  - finite command writes publish through OCC and are readable.
  - finite `tty=false` `nohup ... 2>&1 &` descendant cleanup leaves no matching
    process.
  - PTY `tty=true` `nohup ... 2>&1 &` descendant cleanup leaves no matching
    process.
- P95 gates:
  - finite `exec_command(tty=false, cmd=true)`: 46.393 ms, gate <= 60 ms.
  - `exec_command(tty=true, cmd=true)`: 48.933 ms, gate <= 100 ms.
  - `check_pty_command_progress`: 1.688 ms, gate <= 20 ms.
  - `write_pty_command_stdin` to visible echo: 57.441 ms, gate <= 100 ms.
  - `cancel_pty_command`: 54.822 ms, gate <= 500 ms.
  - cancel cleanup: 403.108 ms, gate <= 2500 ms.
- Load matrix: passed at 1/3/5/10 concurrency for finite no-op, finite write,
  and PTY no-op operations.

### Final Local Verification

- `cargo fmt --all --check && cargo test -p eos-layerstack -p eos-daemon -p eos-overlay && cargo check -p eos-daemon --target x86_64-unknown-linux-musl`:
  passed with pre-existing warnings in adjacent crates.
- `.venv/bin/python -m ruff check backend/src/sandbox/daemon/paths.py backend/scripts/bench_rust_daemon_phase2.py backend/scripts/bench_rust_daemon_phase3.py backend/scripts/bench_rust_daemon_phase3t_pty.py backend/src/sandbox/layer_stack/changes.py backend/src/sandbox/layer_stack/layer_index.py backend/src/sandbox/layer_stack/view.py backend/tests/live_e2e_test/sandbox/workspace_base/test_base_import_cost.py backend/src/sandbox/host/runtime_artifact/__init__.py`:
  passed.
- `.venv/bin/python -m pytest -q backend/tests/unit_test/test_sandbox/test_layer_stack/test_workspace_base.py`:
  5 passed.

### Notes

- Daytona was not run.
- Live runs used the existing Docker setup: `EOS_DOCKER_PRIVILEGED=1`, the
  existing `sweevo-dask__dask-10042:latest` image, and the repo tier/bench
  scripts.
- Rust daemon isolated-workspace public ops are still not registered in this
  phase, so the final Rust-runtime comparison is between shared ephemeral
  command paths. Existing Python isolated-workspace tests remain separate live
  coverage for Python daemon isolated mode.
