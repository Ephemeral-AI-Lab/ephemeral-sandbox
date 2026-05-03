# Phase 6 — daemon-local overlay fold: Implementation Report

Companion to
[`phase-06-fold-daemon-overlay-stages.md`](./phase-06-fold-daemon-overlay-stages.md).
Records the daemon-local overlay execution fold, result-envelope marker,
parity coverage, live performance run, and remaining follow-ups.

---

## 1. Verdict

**Verdict: ships.** Phase 6 moved the daemon `svc.cmd` overlay hot path from
two outer subprocess stages (`git_snapshot` + `run_overlay`) to one
daemon-local `unshare -Urm` subprocess. The runtime now builds the Git
snapshot in namespace via `--snap ""`, writes `result.json` atomically as
the completion marker, and the auditor reads `result.json`, `stdout.bin`,
and `diff.ndjson` through local Python file I/O before committing through
the existing OCC path.

The live Daytona E2E passed against `dask__dask_2023.3.2_2023.4.0`:

| metric | Phase 3.5 rebase baseline | Phase 6 live | result |
|---|---:|---:|---:|
| 1x `svc_cmd` p50 | 7.284s | 6.735s | 8% faster |
| 5x `svc_cmd` p50 | 2.458s | 1.667s | 32% faster |
| 10x `svc_cmd` p50 | 2.614s | **1.805s** | **31% faster; below 2.0s gate** |

The live structural check also passed: **16 daemon-local unshare subprocess
starts for 16 `svc.cmd` ops**, and **0 separate `git_snapshot` auditor-stage
starts** in the captured daemon log.

---

## 2. Scope Delivered

| Task | Verdict | Evidence |
|---|---|---|
| 6.0 verify stage scope | PASS | Live 10x post-fold stage p50: `read_stdout=0.000049s`, `read_diff=0.000114s`, `cleanup=0.000365s`; these remain pure local file I/O. Scope taken: fold `git_snapshot` into the unshare invocation and keep local read/diff/cleanup. |
| 6.1 atomic `result.json` | PASS | `overlay/runtime/runner.py` writes `result.json` via temp file + `os.replace` after `diff.ndjson` for both normal and policy-reject outcomes. |
| 6.2 daemon-local branch | PASS | `OverlayAuditor.execute` branches only when `daemon_local=True`, `sandbox is None`, and `on_progress_line is None`; the multi-stage/streaming path remains available. |
| 6.3 daemon-side wiring | PASS | `daemon/server.py:_build_service(...)` passes `daemon_local=True`; `CodeIntelligenceService -> InProcessBackend -> AuditedCommandExecutor -> OverlayAuditor` threads the flag explicitly. |
| 6.4 result-shape parity | PASS | New five-case parity test covers gitinclude, gitignore, mixed, aborted-version, and policy-reject outcomes. |
| 6.5 internal bash-wrap stripping | PASS for daemon-local hot path | Daemon-local path does not call `_do_exec`; its only subprocess is `subprocess.run([...], shell=False)` for `unshare`. |
| 6.6 live E2E | PASS | `test_live_ci_phase6_svc_cmd_fold.py` passed in 68.29s with detailed per-op/per-stage logs. |
| 6.7 regression | PASS | `backend/tests/test_sandbox/ backend/tests/test_tools/`: 724 passed, 2 skipped. |

---

## 3. File Inventory

### Source

| Path | Change |
|---|---|
| `backend/src/sandbox/code_intelligence/overlay/runtime/runner.py` | Adds `_write_result_json(...)` and writes the atomic completion marker after `diff.ndjson`. |
| `backend/src/sandbox/code_intelligence/overlay/auditor.py` | Adds the daemon-local branch and helpers for one `unshare` subprocess, result-envelope read, local stdout/diff read, OCC commit, cleanup, and DEBUG subprocess-count logs. |
| `backend/src/sandbox/code_intelligence/overlay/command_executor.py` | Threads `daemon_local` to lazily constructed `OverlayAuditor`. |
| `backend/src/sandbox/code_intelligence/backends/` | Adds `daemon_local` to `InProcessBackend` and passes it to `AuditedCommandExecutor`. |
| `backend/src/sandbox/code_intelligence/service.py` | Adds the explicit `daemon_local` constructor path for in-process services. |
| `backend/src/sandbox/code_intelligence/daemon/server.py` | Constructs the daemon-resident service with `daemon_local=True`. |

### Tests and Performance Artifacts

| Path | Change |
|---|---|
| `backend/tests/test_sandbox/test_code_intelligence/test_overlay_daemon_local_parity.py` | New five-case parity corpus. |
| `backend/tests/test_sandbox/test_code_intelligence/test_overlay_run.py` | Adds atomic `result.json` marker test. |
| `backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py` | New live 1x/5x/10x performance and structural daemon-log E2E. |
| `backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T19-43-36Z.json` | Live Phase 6 timing output. |

---

## 4. Performance Evaluation

Live command:

```bash
uv run pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
```

Result:

```text
1 passed in 68.29s
timing json: backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T19-43-36Z.json
```

### 4.1 Headline Latency

| distribution | p50 | p95 | p99 | samples |
|---|---:|---:|---:|---:|
| `svc_cmd_1x_latency` | 6.735s | 6.735s | 6.735s | 1 |
| `svc_cmd_5x_latency` | 1.667s | 1.673s | 1.673s | 5 |
| `svc_cmd_10x_latency` | **1.805s** | 1.825s | 1.825s | 10 |

The 10x p50 gate is `< 2.0s`; measured 10x p50 is **1.805s**.

### 4.2 Daemon-Local Stage Breakdown at 10x

| stage | p50 | p95 | interpretation |
|---|---:|---:|---|
| `unshare` | 1.292s | 1.304s | one in-namespace process containing snapshot + overlay + user command + classify |
| `read_envelope` | 0.0002s | 0.0002s | local `result.json` read |
| `read_stdout` | 0.00005s | 0.00057s | local `stdout.bin` read |
| `read_diff` | 0.00011s | 0.00025s | local `diff.ndjson` read |
| `commit` | 0.0018s | 0.0067s | OCC commit in process |
| `cleanup` | 0.00037s | 0.0052s | local `shutil.rmtree` |
| `total` | 1.298s | 1.306s | daemon-side overlay stage total |

Resource samples during the run stayed flat enough for this phase:
`rss_growth_mb=0.75`, `fd_growth=0`.

### 4.3 Structural Evidence

The live E2E captured daemon-log bytes from immediately before the
1x/5x/10x `svc.cmd` batches and counted:

```text
daemon_local_unshare_subprocess_count = 16
daemon_local_git_snapshot_stage_count = 0
```

That matches the expected 16 total operations across 1 + 5 + 10
concurrency and verifies that Phase 6 removed the separate auditor
`git_snapshot` subprocess stage from the daemon-local path.

---

## 5. Verification Commands

```bash
uv run pytest \
  backend/tests/test_sandbox/test_code_intelligence/test_overlay_daemon_local_parity.py \
  backend/tests/test_sandbox/test_code_intelligence/test_overlay_run.py::test_write_result_json_is_atomic_completion_marker \
  backend/tests/test_sandbox/test_code_intelligence/test_overlay_auditor.py::test_local_daemon_readback_uses_filesystem_without_exec \
  -q
# 7 passed
```

```bash
uv run pytest \
  backend/tests/test_sandbox/test_code_intelligence/test_overlay_auditor.py \
  backend/tests/test_sandbox/test_code_intelligence/test_overlay_dispatch.py \
  backend/tests/test_sandbox/test_code_intelligence/test_backends.py \
  backend/tests/test_sandbox/test_code_intelligence/test_daemon_dispatch.py \
  backend/tests/test_sandbox/test_code_intelligence/test_daemon_server.py \
  -q
# 63 passed
```

```bash
uv run pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q
# 724 passed, 2 skipped
```

```bash
uv run ruff check \
  backend/src/sandbox/code_intelligence/overlay/auditor.py \
  backend/src/sandbox/code_intelligence/overlay/command_executor.py \
  backend/src/sandbox/code_intelligence/backends/ \
  backend/src/sandbox/code_intelligence/service.py \
  backend/src/sandbox/code_intelligence/daemon/server.py \
  backend/src/sandbox/code_intelligence/overlay/runtime/runner.py \
  backend/src/sandbox/code_intelligence/overlay/runtime/__init__.py \
  backend/tests/test_sandbox/test_code_intelligence/test_overlay_daemon_local_parity.py \
  backend/tests/test_sandbox/test_code_intelligence/test_overlay_run.py \
  backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py
# All checks passed
```

```bash
git diff --check
# no output
```

Targeted mypy over `backend/src/sandbox/code_intelligence` was also tried,
but that package is outside the repo's strict mypy target and still has
pre-existing failures unrelated to Phase 6: `SymbolKind.OTHER`, an unused
ignore, and the `CodeIntelligenceBackend.is_initialized` protocol shape.

---

## 6. Observed Follow-Up

The live daemon log still emitted non-strict `WORKSPACE WRITE BYPASS`
messages for some committed `svc_cmd` paths. The request results themselves
were successful OCC commits (`git_commit_status="committed"`), and the live
E2E passed. This looks like guard accounting/noise rather than a Phase 6
hot-path failure, so it is left as a separate follow-up instead of being
mixed into this performance phase.

If the guard is meant to be strict for `svc_cmd`, the next focused task should
inspect the daemon bypass guard's ledger-window comparison under concurrent
overlay commits.

---

## 7. Remaining Work

1. Keep the Phase 6 timing JSON as the new `svc.cmd` perf claim of record.
2. Investigate the non-strict bypass-guard log noise under concurrent
   `svc_cmd` commits.
3. Once the multi-stage fallback is no longer needed for streaming or
   non-daemon callers, delete the old auditor body in a separate cleanup.
