# Shell `background=True` — Phase 2 validation + audit wiring

Status: refined draft (round 2). Successor to `2026-05-22-shell-background-mode.md`.

## What changed vs the round-1 draft of this file

- **Step 1 redesigned.** Round 1 proposed either an "AuditSink analogue from
  `publish_operation_*`" (Path A) or "direct JSONL append" from inside the
  daemon (Path B). Both paths were architecturally wrong:
  - `publish_operation_*` is *host-side only* — its only call site is
    `backend/src/sandbox/api/tool/core/audit.py:31` via `audited_operation`.
    No daemon module imports `AuditSink` or calls into it.
  - The daemon runs *inside* the sandbox container; it does not know the
    host's audit `run_dir` and cannot write `sandbox_events.jsonl` directly.
  - The correct pattern is already in the tree: `sandbox_events_from_tool_completion()`
    at `backend/src/task_center_runner/audit/sandbox_events.py:32` derives
    sandbox events host-side from `ToolExecutionCompleted.metadata`.
  - Phase 2 mirrors that pattern: host-side derivation in
    `_shell_background_dispatch`, daemon's `audit_callback` stays test-only.
- **AC-1 microbench dropped.** Original definition ("≤ 50 ms tool return"
  measured for `shell.launch` round-trip) is inconsistent with the codebase's
  measured cost — `overlay_run ≈ 0.43 s` per the
  [`codeact_overlay_cost_breakdown`](../../.../memory) memory. The
  `shell.launch` RPC includes lease acquire (manifest pin + base-hash freeze)
  and cannot meet 50 ms p95. Redefined as a derived check in T1: assert the
  engine-side `shell()` coroutine yields with a `ShellResult` placeholder
  within 50 ms *of the daemon's launch ack returning*; otherwise drop the AC.
- **T4 / T5 feasibility surfaced.** Requires (i) configurable TTL on the
  registry singleton (default 300 s is unusable in CI) and (ii) confirmed
  separation between dispatcher executor and `ShellExecutor`. Both become
  named sub-steps below.
- **T9 dropped.** `backend/tests/unit_test/test_sandbox/test_shell_job_registry.py:259-305`
  already asserts the registry's `audit_callback` shape (launched + reaped
  + cancelled). T10 covers the production path on its own.
- **One-commit-per-scenario relaxed.** Group commits by shared probe:
  T1 + T2 + T3 share the seed probe; T6 + T7 + T8 share the long-shell +
  cancel-timing harness. T4 and T5 stay alone (engine-kill rig; high-fan-out
  rig).

## Premise (unchanged)

Phase 1 (2026-05-22) shipped the daemon-native job control surface
(`shell.launch / poll / cancel / reap`) plus engine glue. Unit coverage is
solid (16 new tests + 5 updated); live coverage is zero. Three deferrals
the merge note called out are the work:

1. `ShellJobRegistry.audit_callback` is never set in production — the four
   `SHELL_*` audit events are silent in the on-disk audit tree.
2. The 8 live integration scenarios (`T1`–`T8`) the round-2 design called
   for are unimplemented; they need real Docker + a probe.
3. `AC-7` (engine-kill TTL reap window), `AC-8` (210-shell executor
   exhaustion), and `AC-9` (cancel during `run_maintenance_after_publish`)
   have no empirical measurement.

Phase 2 closes these gaps without re-touching the daemon code path that
Phase 1 verified at the unit level.

## Principles

1. **Validate-before-extend.** Land the live scenarios + audit wiring before
   any new control-plane verbs (progress streaming, fan-out to other tools).
2. **One commit per coherent scenario family** (not per test): keeps diffs
   reviewable, failures of one family must not block the others.
3. **Empirical gates.** Replace "should be ≤ X" with measured assertions
   driven by the existing
   `.agents/skills/sandbox-performance-evaluation/scripts/summarize_sandbox_perf.py`.
4. **No daemon code drift.** If a scenario surfaces a daemon bug, file it as
   a follow-up phase rather than expanding scope here. Exception: the two
   config knobs the daemon needs to make T4 / T5 testable (§Step 4).

## Decision drivers

1. Phase 1 unit tests cover the registry mechanics but no test crosses the
   host ↔ daemon socket boundary. The audit-event invariant (AC-5: exactly
   one `SHELL_REAPED` per `SHELL_LAUNCHED`) can only be verified once the
   audit derivation is wired.
2. The plan's "follow-ups" list (stdout streaming, generalization to other
   tools) is *additive* surface. Doing it before the live tests would mean
   measuring a moving target.
3. The TTL reaper (Phase 1, Step 1) has the largest blast radius and the
   smallest unit-test coverage — only the `_reap_stale_jobs` happy path. An
   engine-kill scenario (T4) is the only way to actually exercise it.

## Implementation steps

### 1. Host-side derivation of `SHELL_*` audit events

**File:** `backend/src/sandbox/api/tool/shell.py` (~30 LOC).
**Why host-side:** see "What changed" §1; the daemon has no path to
`sandbox_events.jsonl`.

- Thread `audit_sink` into `_shell_background_dispatch` via the function
  signature (currently it captures `selected_transport` from the outer
  closure but not `audit_sink`). Single signature change at
  `shell.py:110-117`; pass-through at `shell.py:66-73`.
- After a successful launch response, emit `SHELL_LAUNCHED` with payload
  `{job_id, lease_id, request_id}` (mirrors the daemon's payload at
  `shell_job.py:178-184`).
- After `shell.reap` returns, emit `SHELL_REAPED` with payload
  `{job_id, status, changed_paths_count}` (mirrors `shell_job.py:295-302`).
- On the cancel path (`_send_cancel_then_reap`), emit `SHELL_CANCELLED`
  with `{job_id, reason="engine_cancel"}` before the cancel RPC; emit a
  trailing `SHELL_REAPED` with whatever `status` the post-cancel reap
  returns (or `status="cancel_reap_failed"` if the reap RPC itself fails).
- Add a single entry to `_SANDBOX_EVENT_MAP` in
  `backend/src/task_center_runner/audit/legacy.py:15-31` mapping each new
  event to a new `EventType.SANDBOX_SHELL_*` in
  `backend/src/task_center_runner/audit/events.py`. Without the map the
  `LegacySandboxAuditSink` drops them.
- Test: extend `backend/tests/unit_test/test_sandbox/test_api/test_shell_background_dispatch.py`
  with a stub `AuditSink` that records published events; assert the
  golden path emits exactly `[LAUNCHED, REAPED]` and the cancel path emits
  `[LAUNCHED, CANCELLED, REAPED]`.

**What the daemon's `audit_callback` remains for:** unit tests of the
registry only (`test_shell_job_registry.py`). It is never set in production
after this phase — production observability flows entirely through the
host-side derivation above.

### 2. Two daemon-side config knobs (only what T4 / T5 require)

These are the *minimum* daemon-side changes that make the integration tests
runnable; no behavior changes for production.

- **TTL override via env var.** `backend/src/sandbox/daemon/service/shell_job.py:527-533`:
  in `get_shell_job_registry`, read `EOS_SHELL_JOB_TTL_S` if set; cast to
  float; fall back to `DEFAULT_TTL_SECONDS`. Required because T4 wants to
  observe TTL fire in CI on a budget < 6 min.
- **Reaper-interval override via env var.** Same site,
  `EOS_SHELL_JOB_REAPER_INTERVAL_S`. Required to keep the T4 wait window
  small.
- **Assert executor separation.** Add a one-line check at registry construction:
  `assert not isinstance(self._executor, type(asyncio.get_event_loop()._default_executor))`
  (or simpler: assert `self._executor is not asyncio.get_event_loop()._default_executor`).
  Backs Pre-mortem #3 and AC-14.

No new audit events, no new RPC verbs.

### 3. Build the shared `background_shell_probe`

**File:** `backend/src/task_center_runner/agent/mock/background_shell_probe.py`
(~150 LOC).
**Pattern:** identical to
`backend/src/task_center_runner/agent/mock/heavy_io_zoned_probe.py:95-138`
(seed) + `:141-268` (worker) + `:271-368` (reconcile).

- `make_background_shell_seed_probe()` seeds the workspace dirs.
- `make_background_shell_worker_probe(*, count, command, cancel_after_s, interleave_foreground)`
  drives the test surface. The probe calls `shell_tool` with
  `background=True` and uses `wait_background_tasks` /
  `check_background_task_result` from `tools.background.*` (existing engine
  surface).
- The probe writes a summary JSON to
  `/testbed/.ephemeralos/sweevo-mock/background_shell/summary.json`; the
  test reads it back via `sandbox_api.read_file` (same pattern as
  `test_heavy_io_zoned_concurrent.py:159-167`).

### 4. Scenarios + integration tests T1 – T3 (golden / cancel / interleave)

**Scenario file:** `backend/src/task_center_runner/scenarios/sandbox/background_shell.py`
(register in `SCENARIO_REGISTRY` like
`scenarios/sandbox/heavy_io_zoned_concurrent.py`). Three scenarios share
the probe with different knobs (`background_shell_golden`,
`background_shell_cancel`, `background_shell_interleave`).

**Test files** under `backend/src/task_center_runner/tests/mock/sandbox/`:
- `test_background_shell_golden.py`
- `test_background_shell_cancel.py`
- `test_background_shell_interleave.py`

Each follows the gating pattern at
`test_heavy_io_zoned_concurrent.py:39-46` (`database_configured()` +
`live_e2e_heavy_enabled()`).

Assertion surface (per the round-2 design table):
- **T1:** 3 launches → `wait_completed`, `workspace_tree_bytes == 0`,
  manifest depth grew by 3, AC-12 invariant holds for this scenario.
- **T2:** cancel at 5 s → next foreground `command_exec.mount_workspace_s`
  < 100 ms; `changed_paths == []`; `/proc/<pid>` gone within 3 s; exactly
  one daemon-side `release_lease` audit entry for that `lease_id`.
- **T3:** 1 × background 30 s + 10 × interleaved foreground; p95 foreground
  `command_exec.mount_workspace_s` unchanged within ±20 % vs the
  no-background baseline already captured by
  `test_heavy_io_zoned_concurrent.py`.

**Commit grouping:** one commit lands the scenario file + probe + 3 tests.

### 5. T4 — engine-kill TTL reaper

**File:** `test_background_shell_engine_kill.py`.

Two-process test:

- Spawn a subprocess that runs the engine driver (re-use the existing
  `run_scenario_on_sweevo_image` entry-point in
  `backend/src/task_center_runner/environments/sweevo_image/fixtures.py:42`
  via `python -m`). There is no current precedent for engine-in-subprocess
  in the live tests — this is new infra. Budget ~80 LOC.
- Probe (running inside the subprocess engine) launches 1 background shell
  with a 60 s sleep; writes `pid_file` to a shared host volume; exits
  after launch.
- Test SIGKILLs the engine subprocess once the pid file appears.
- Set `EOS_SHELL_JOB_TTL_S=10` and `EOS_SHELL_JOB_REAPER_INTERVAL_S=2` in
  the daemon env for this scenario (Step 4 enables this).
- Assert (via daemon's `api.layer_metrics` RPC, NOT the engine's
  `sandbox_events.jsonl` — the engine is dead): the lease count returns
  to baseline within `ttl + 30 s`. Foreground op from a fresh engine
  succeeds afterward.

**AC-13 verification path:** since the engine's `sandbox_events.jsonl`
writer dies with the engine, AC-13 is verified by polling
`api.layer_metrics` (existing op at `dispatcher.py:203`) for the lease
count to drop to its pre-launch value. Add a TTL-reap counter to
`ShellJobRegistry` and expose it via a new daemon RPC `api.shell.metrics`
that returns `{ttl_reaped_total, active_jobs}`. ~20 LOC daemon side; lifts
AC-13 from "engine audit log" to "daemon lease counter".

### 6. T5 — executor exhaustion

**File:** `test_background_shell_executor_exhaustion.py`.

- Probe launches 80 background shells with `sleep 60` (was 210 in the
  round-2 plan; reduced because the dedicated `ShellExecutor` default is
  `DEFAULT_EXECUTOR_WORKERS = 64` at `shell_job.py:56` — 80 saturates the
  pool with queue overflow without saturating SWE-EVO Docker quotas).
- Probe cancels all 80 jobs in parallel.
- Probe issues a single foreground `read_file` against a tracked path and
  records the dispatch latency.
- Assert: foreground `read_file` completes in < 1 s; this is the
  executor-isolation invariant (AC-14). Add a daemon-side assertion via
  `api.shell.metrics` that `active_jobs` drops to 0 within 30 s of the
  cancel fan-out.

### 7. T6 – T8 — cancel edge cases

**Files:**
- `test_background_shell_partial_write_cancel.py`: `dd of=tracked.bin bs=1M count=200`
  + cancel at 5 s → no `tracked.bin` in workspace OCC; upperdir discarded.
  This is the OCC-skip-on-cancel invariant at `shell_job.py:262-276`.
- `test_background_shell_cancel_during_maintenance.py`: cancel arriving
  during `run_maintenance_after_publish` → OCC consistent, no orphan
  manifest fragment. Drives the `reap` path at `shell_job.py:262-281`
  where cancel after `publish_cycle` is a no-op.
- `test_background_shell_late_cancel_race.py`: 1 s shell + 1.2 s sleep +
  cancel → exactly one terminal status; `check_background_task_result`
  returns the real `ShellResult`. Live counterpart to
  `test_late_cancel_after_completion_preserves_status` at
  `test_shell_job_registry.py:386-409`.

**Commit grouping:** one commit lands the shared cancel-timing harness +
all three tests.

### 8. Per-scenario `sandbox_events.jsonl` invariant scan

**No separate test file** (round-1 had T10 as a meta-test; this consumes a
test slot for what's a one-helper-call). Add a shared helper
`_assert_shell_audit_invariants(report.run_dir / "sandbox_events.jsonl")`
in `backend/src/task_center_runner/tests/mock/sandbox/_background_shell_invariants.py`,
called from the teardown of T1 – T8. Asserts:

- `count(SHELL_LAUNCHED) == count(SHELL_REAPED)` for this run.
- Every `SHELL_CANCELLED` has a matching `SHELL_REAPED` by `job_id`.
- No `internal_error`, `stale lowerdir`, `mount_failed`, or
  `manifest references missing layer` lines (Phase 1 AC-11).

T4 is an exception: its run-dir's `sandbox_events.jsonl` is truncated
because the engine was killed; the helper accepts an `expect_truncated=True`
mode that skips the count-equality check and asserts only "no error lines
before the truncation point."

## Pre-mortem (refreshed)

1. ~~"Audit callback fires from the wrong thread."~~ **Moot** under
   host-side derivation (Step 1). The host's `_shell_background_dispatch`
   runs on the engine's asyncio loop, same thread as `audited_operation`.
2. **TTL reaper races with normal reap.** T4 kills the engine; the ongoing
   `shell.reap` RPC dies with the host; the daemon's reaper sees
   `last_poll_at` aging and fires a second cleanup.
   - Mitigation: `OperationOverlayHandle._released` (Phase 1) gates double
     release. Verify by `api.shell.metrics` showing `ttl_reaped_total == 1`
     in T4, not 2.
3. **Executor exhaustion deadlocks the daemon.** T5's 80 launches exceed
   the default 64-worker pool. If the dispatcher uses the same pool, new
   RPCs queue behind shell launches → deadlock under cancel.
   - Mitigation: assert the daemon's main RPC executor is *not* the
     `ShellExecutor` (Step 4 sub-step).
4. **Maintenance-during-cancel skips manifest cleanup.** T7 cancels mid
   `run_maintenance_after_publish`.
   - Mitigation: `reap` at `shell_job.py:262-276` already gates publish on
     `not cancelled and exit_code is not None`. Test asserts
     `SHELL_REAPED.changed_paths_count == 0` for the cancelled job.
5. **`SHELL_LAUNCHED` emit precedes the daemon's `_run_strategy` thread
   spawning the child.** Host-side derivation fires the event when
   `shell.launch` RPC returns, but the daemon may still be inside its
   `ThreadPoolExecutor.submit` → child not yet forked.
   - Mitigation: tests must NOT assume `pid_alive` after `SHELL_LAUNCHED`.
     The contract is "lease acquired, future submitted." `pid_alive`
     becomes observable via `shell.poll` only.
6. **Engine-in-subprocess test rig (T4) lacks precedent.** Fixture
   complexity could explode.
   - Mitigation: timebox to 80 LOC; if it doesn't fit, descope T4 to a
     unit test against `ShellJobRegistry` with `ttl_seconds=0.5` and a
     mocked overlay (already buildable from `test_shell_job_registry.py`'s
     `_FakeSandboxOverlay`). The "daemon survives engine-kill" guarantee
     is *architectural* (the daemon is a separate process); the
     integration test is verification, not the load-bearing proof.

## Tests (catalog)

| # | Test | Asserts |
|---|---|---|
| T1 | `test_background_shell_golden.py` | golden launch + wait + check |
| T2 | `test_background_shell_cancel.py` | cancel; lease released; no leaked upperdir |
| T3 | `test_background_shell_interleave.py` | background + foreground p95 mount latency unchanged |
| T4 | `test_background_shell_engine_kill.py` | TTL reaper fires within `ttl + 30 s`; AC-13 via `api.shell.metrics` |
| T5 | `test_background_shell_executor_exhaustion.py` | 80 cancelled shells; foreground read < 1 s; AC-14 |
| T6 | `test_background_shell_partial_write_cancel.py` | cancelled partial write absent from OCC |
| T7 | `test_background_shell_cancel_during_maintenance.py` | OCC consistent through cancel-during-maintenance |
| T8 | `test_background_shell_late_cancel_race.py` | exactly one terminal status; real result preserved |
| U1 | extend `test_shell_background_dispatch.py` | host-side derivation emits `[LAUNCHED, REAPED]` golden + `[LAUNCHED, CANCELLED, REAPED]` cancel |
| U2 | new `test_shell_job_registry_env_overrides.py` | `EOS_SHELL_JOB_TTL_S` / `EOS_SHELL_JOB_REAPER_INTERVAL_S` honored |

T9 / T10 / T11 from the round-1 draft of this file are dropped — see
"What changed."

## Acceptance criteria

Phase 1 acceptance criteria (AC-1 through AC-11) are inherited; this phase
*operationalizes* them via the live tests above. New criteria for Phase 2:

| # | Criterion | Measured via |
|---|---|---|
| AC-12 | `SHELL_LAUNCHED` / `SHELL_REAPED` count equality across T1 – T8 (T4 exempted per §8) | per-test invariant helper |
| AC-13 | Daemon survives engine-kill: lease count returns to baseline within `ttl + 30 s`; `api.shell.metrics.ttl_reaped_total` increments by 1 | T4 |
| AC-14 | Daemon RPC executor and `ShellExecutor` are distinct threadpools; 80 cancelled shells do not block a follow-up foreground op > 1 s | T5 + Step 4 assertion |

The original AC-1 ("≤ 50 ms tool return") is **dropped** — it was
under-defined and the codebase's measured costs make 50 ms p95 implausible
for a full `shell.launch` round-trip. The intent (background should not
block the agent's event loop) is preserved by T3's interleave assertion.

## Verification

```bash
set -a; source .env; set +a

# Stage 1 — unit (host-side derivation, env overrides, regression on Phase 1)
uv run pytest -q -x --tb=short \
  backend/tests/unit_test/test_sandbox/test_api/test_shell_background_dispatch.py \
  backend/tests/unit_test/test_sandbox/test_shell_job_registry.py \
  backend/tests/unit_test/test_sandbox/test_shell_job_registry_env_overrides.py \
  backend/tests/unit_test/test_engine/test_background_terminal_latch.py

# Stage 2 — integration: shared probe + golden / cancel / interleave
uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_golden.py \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_cancel.py \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_interleave.py

# Stage 3 — integration: engine kill + executor exhaustion
uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_engine_kill.py \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_executor_exhaustion.py

# Stage 4 — integration: cancel edge cases
uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_partial_write_cancel.py \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_cancel_during_maintenance.py \
  backend/src/task_center_runner/tests/mock/sandbox/test_background_shell_late_cancel_race.py

# Stage 5 — perf summarization across all 8 live scenarios
for D in $(ls -td .sweevo_runs/scenario_logs/sandbox.background_shell_* 2>/dev/null | head -8); do
  python3 .agents/skills/sandbox-performance-evaluation/scripts/summarize_sandbox_perf.py "$D"
done
```

## ADR

- **Decision**: ship 8 live scenarios + host-side `SHELL_*` audit derivation
  + minimum-viable daemon config knobs in one phase, before any
  control-plane extension.
- **Drivers**: Phase 1's daemon code path has unit coverage but zero live
  coverage; the audit events are silent; the TTL reaper has never fired in
  anger.
- **Alternatives considered**:
  - *Daemon-side audit emit (round-1 draft Path A / Path B)* — rejected
    with file:line evidence: `publish_operation_*` is host-side only;
    direct JSONL append crosses the sandbox container boundary.
  - *Land stdout streaming first* — rejected: streaming extends
    `shell.poll`, which is only exercised by unit tests today. Adding
    streaming on an unverified base compounds risk.
  - *Generalize to other tools* — same reason. Phase 1's ADR follow-ups
    list this; the prerequisite is a verified base.
- **Consequences**:
  - One host-side derivation (~30 LOC `shell.py` + 1 legacy.py mapping
    entry + 1 events.py constant family).
  - Two daemon env-var knobs (~10 LOC `shell_job.py`).
  - One probe module (~150 LOC).
  - One new daemon RPC `api.shell.metrics` (~20 LOC) for AC-13 visibility.
  - 8 integration tests + 2 unit tests (~600 LOC total, gated by
    `live_e2e_heavy_enabled`).
- **Follow-ups** (Phase 3 candidates):
  - `shell.poll` stdout streaming wired into `check_background_task_result`.
  - Generalize `shell.launch / poll / reap` to other long-running tools.
  - Per-sandbox `ShellExecutor` sizing surfaced as `ephemeralos.yaml` knob
    (currently env-only).

## Risk register

- **R1 (medium)**: Live tests are flaky under shared SWE-EVO image quota.
  Mitigation: each test asserts on audit + summary JSON, not timing
  variance > 100×. Retry-on-flake budget is a separate phase.
- **R2 (medium)**: T4's engine-in-subprocess fixture lacks precedent in
  the live test suite. Mitigation: timebox to 80 LOC; if blown, descope to
  a unit test against `ShellJobRegistry` with `ttl_seconds=0.5` — the
  daemon-survives-engine-kill property is architectural, not behavioral.
- **R3 (low)**: SHELL_LAUNCHED ordering — host fires the event before the
  daemon's strategy thread spawns the child. Mitigation: documented as
  Pre-mortem #5; tests must not assert `pid_alive` immediately after
  launch.
- **R4 (low)**: Host-side cancel emit relies on `_send_cancel_then_reap`
  being reached — if the engine task is cancelled before launch returns,
  no event fires. Mitigation: emit `SHELL_CANCELLED` only inside the
  `except asyncio.CancelledError` branch at `shell.py:160-166`; this is
  semantically correct (no job to cancel if launch never completed).

## Review provenance

- Phase 1 implementation summary (Yifan, 2026-05-22): unit tests pass,
  T1 – T8 explicitly deferred, AC-7 / AC-8 / AC-9 not measured.
- Phase 1 advisor sign-off (round 2): "ship-ready modulo the engine-glue
  test" — engine-glue test landed in `test_shell_background_dispatch.py`.
- Round-1 draft of this file (2026-05-22, same path): advisor flagged
  daemon-side audit emit as architecturally wrong; this round corrects
  it. AC-1 was dropped on the same review.
- Next-phase scope agreed: validation + audit derivation before any new
  control-plane verbs.
