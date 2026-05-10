# OCC Auto-Squash Optimization — Experiment Plan

Generated: 2026-05-11
Parent report: `.omc/plans/occ-layer-stack-commit-resume-auto-squash-report-20260511.md`
Verification harness: `.omc/plans/occ-auto-squash-perf-verification-test-plan-20260511.md`
Baseline numbers: `.omc/perf/baselines/2026-05-11/`
Aggregator: `backend/scripts/perf/auto_squash_compare.py`
Status: **Draft — implementation work, gated by the verification harness already in place.**

## Purpose

Decide which of three optimization candidates — and which combination — gives the best edit/write latency reduction at acceptable behavioral risk. Each candidate is tested under the same 5-scenario harness and the same per-tool / per-key timing aggregator, so all decisions are directly comparable to the canonical sync-baseline numbers from 2026-05-11.

## Bottleneck recap (what the baseline shows)

On `full_case_user_input` (high-pile-up scenario):

| Tool | p95 user-facing | `commit_resume_wait_s` p95 | share of user latency |
|---|---:|---:|---:|
| `write_file` | 3,156.6 ms | 2,653.3 ms | 84% |
| `edit_file` | 3,100.2 ms | 2,505.5 ms | 81% |

`commit_resume_wait_s` and `auto_squash.total_s` totals match within 0.1% (150,317 vs 150,193 ms). 60 of 82 squash events have `raced=1`. Synchronous post-publish auto-squash dominates user-facing edit/write latency, full stop.

## Three candidates under test

| Candidate | Flag | Default | Touches | Behavior risk |
|---|---|---|---|---|
| H1: Coalesced squash worker | `EOS_OCC_SQUASH_MODE=coalesced` | off | scheduler only — squash still on critical path, but skip-if-running | low |
| H2: Async squash | `EOS_OCC_SQUASH_MODE=async` | off | squash leaves critical path, runs on a workspace background worker | medium–high (failure semantics) |
| H3: Higher AUTO_SQUASH_MAX_DEPTH | `EOS_OCC_AUTO_SQUASH_MAX_DEPTH=N` | 32 | constant tweak, no scheduling change | low (memory/manifest size) |

The three flags are orthogonal. The plan tests them individually, then exercises the two most promising combinations.

---

## Hypothesis 1 — Coalesced squash worker

### Statement

> If multiple publishes cross `AUTO_SQUASH_MAX_DEPTH` while a squash is already in flight, only the first one needs to do work. Coalescing eliminates `raced=1` re-attempts and the redundant CAS contention they cause, reducing per-call wait without moving squash off the critical path.

### Predicted impact

- `layer_stack.auto_squash.raced` count drops from ~73% of events to ≈0.
- `commit_resume_wait_s` p95 on `full_case_user_input` drops by ≥30% (target: 2,653 ms → ≤1,857 ms on `write_file`).
- `auto_squash.total_s` per-event p95 stays the same (we still squash; we just don't repeat).
- `api.{write,edit}.total_s` p95 drops by ≥20%.
- Manifest depth max may grow modestly (e.g., 44 → ≤64) because back-pressure relaxes.

### Implementation surface

`backend/src/sandbox/occ/service.py`:

- Per-workspace state: `(asyncio.Lock | None, pending_recheck: bool)` keyed by workspace id.
- `_auto_squash_after_publish_sync` becomes:
  - If lock held: set `pending_recheck=True`, return immediately (publish-only path).
  - Else: take lock, run `squash(max_depth=32)`, on exit clear lock; if `pending_recheck` was set during the run, re-read manifest depth once and trigger another squash only if still over threshold.
- New timing keys to emit:
  - `layer_stack.auto_squash.skipped_in_flight` (1.0 when this call short-circuited).
  - `layer_stack.auto_squash.recheck_triggered` (1.0 when post-run re-check fired another squash).

### Probe steps

1. Implement flag-gated coalesced path; default remains the current synchronous behavior.
2. Run unit tests:
   - `backend/tests/unit_test/test_sandbox/test_occ/test_auto_squash.py` — must pass with flag off.
   - Add a new unit test: under flag-on, simulate two publishes crossing threshold concurrently; assert exactly one squash runs and pending re-check fires.
3. Run the 5-scenario harness twice (flag off, flag on). Capture metrics under `.omc/perf/coalesced/2026-MM-DD/`.
4. Aggregate with `auto_squash_compare.py compare`.

### Success criteria (gate)

- Behavior: all 7 paired-test assertions in the verification harness pass under flag on.
- Perf: on `full_case_user_input`, `commit_resume_wait_s` p95 drops by ≥30% **and** `auto_squash.raced` count drops to ≤5% of events.
- Bound: `auto_squash.depth_before` max stays ≤ 64.

### Failure modes to flag

- If `pending_recheck` re-check fires forever (publish rate > squash rate), depth balloons. Add a hard cap: if depth exceeds 2×`AUTO_SQUASH_MAX_DEPTH`, fall back to synchronous waiting on the in-flight squash and treat as a backpressure event.
- Lease safety unchanged — coalescing doesn't change which layers are addressable.

---

## Hypothesis 2 — Async squash

### Statement

> Squash is maintenance, not part of the user-visible commit. If we publish the layer, return the tool result, and run squash on a workspace-scoped background worker, the user-facing edit/write critical path is freed of the entire squash cost.

### Predicted impact

- `commit_resume_wait_s` p95 on `full_case_user_input` drops by ≥80% (target: 2,653 ms → ≤500 ms).
- `api.{write,edit}.total_s` p95 drops to roughly the no-squash floor: ~500 ms (matches the focused probe baseline of 581 ms p95 with squash but no piling).
- `auto_squash.total_s` total stays the same — we still do the same maintenance, just off-path.
- New asynchronous failure surface: squash failure is no longer a tool error.

### Implementation surface

`backend/src/sandbox/occ/service.py`:

- Per-workspace `asyncio.Queue` + a single `asyncio.Task` worker spawned lazily on first publish.
- Worker drains the queue; for each entry, takes the existing lock, runs squash, and emits the same `SANDBOX_LAYER_STACK_LAYERS_SQUASHED` event.
- `commit_prepared(...)` enqueues the post-publish trigger and returns immediately.
- New event type: `SANDBOX_AUTO_SQUASH_MAINTENANCE_FAILED` carrying the exception payload.
- New timing keys:
  - `layer_stack.auto_squash.enqueue_to_run_lag_s` — wall time between publish-return and squash actually starting.
  - `layer_stack.auto_squash.maintenance_error` (1.0 on failure).
- Lifecycle: on workspace teardown, wait up to N seconds for the queue to drain; if it doesn't, log and proceed.

### Required behavior contract changes (these MUST be written and approved before async can claim PASS)

1. Squash failure does not fail the originating tool call. Failures emit `SANDBOX_AUTO_SQUASH_MAINTENANCE_FAILED` and a structured monitor record. Tool result is still `success`.
2. Workspace teardown drains pending squashes for up to 10 s before forcing exit; remaining queue depth is recorded.
3. Snapshot leases held during async squash see the same frozen view they do today (no change).
4. Concurrent commit during async squash uses the same CAS-safe manifest update path; squash retries / skips on CAS conflict.
5. Public tool payloads — `status`, `changed_paths`, `conflict_reason`, `bytes_written`, etc. — keep their meaning.

### Probe steps

1. Land the contract document above as `.omc/plans/occ-auto-squash-async-contract-20260511.md` and get sign-off **before** writing async code.
2. Implement flag-gated async path. Default remains synchronous.
3. Add unit tests:
   - Async-mode squash failure: tool result success, monitor event present, no exception leaks.
   - Workspace teardown with pending squash: drains within budget, records final depth.
   - Concurrent commit during squash: CAS retry succeeds.
4. Run the 5-scenario harness twice (flag off, flag on). Capture metrics under `.omc/perf/async/2026-MM-DD/`.
5. Aggregate with `auto_squash_compare.py compare`. Cross-check the 7 behavior-equivalence invariants from the verification plan §93–115 — these are the gate.

### Success criteria (gate)

- Behavior: all 7 invariants pass byte-equal AND the new failure-surface contract is met.
- Perf: on `full_case_user_input`, `commit_resume_wait_s` p95 drops by ≥80% AND `api.write.total_s` / `api.edit.total_s` p95 each drop by ≥70%.
- Bound: under sustained mutation, `auto_squash.depth_before` max stays ≤ 96 (3× the threshold). If async lags persistently, the queue is the new bottleneck.
- Maintenance-error visibility: at least one test case forces a squash failure and asserts the monitor event surfaces it.

### Failure modes to flag

- Lease invalidation: squash must not delete a layer a live lease can address. Existing lease pinning logic must already cover this; verify with `test_lease_pinning.py`.
- Sandbox teardown losing maintenance work — accepted as a known soft failure (we record what was lost, we don't crash).
- Failure-mask: a real squash bug now hides behind a monitor event instead of a tool error. Escalation policy must be defined: how many `MAINTENANCE_FAILED` events before we surface to the user?

---

## Hypothesis 3 — Higher `AUTO_SQUASH_MAX_DEPTH`

### Statement

> Squash cost is amortized over the publishes that fall within the threshold. Raising the threshold from 32 to 64 (or 128) means fewer mutations trigger squash; the per-event cost grows, but the total of (events × cost) per scenario is roughly conserved or smaller.

### Predicted impact

- `auto_squash` event count on `full_case_user_input` drops from ~82/75 to ~half.
- Per-event `auto_squash.total_s` p95 grows by ~30–60% (more layers to coalesce).
- `api.{write,edit}.total_s` p95 drops modestly (≥10% target) because most calls don't hit squash at all.
- Manifest size grows; memory pressure grows; cold-recovery work grows.
- This is the **simplest** change — one constant — but probably just shifts the problem.

### Implementation surface

- `backend/src/sandbox/occ/service.py`: read `AUTO_SQUASH_MAX_DEPTH` from `os.environ.get("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", "32")` once at module load (or per-instance), default 32.
- No scheduling changes.

### Probe steps

1. Make the constant env-configurable.
2. Run the 5-scenario harness three times: depth=32 (baseline), depth=64, depth=128. Capture under `.omc/perf/depth/2026-MM-DD/{32,64,128}/`.
3. Aggregate with `auto_squash_compare.py compare` for each pair (32↔64, 32↔128).
4. Capture memory pressure separately:
   - `ps -o rss=` on the sandbox daemon process at run end.
   - Manifest file size on disk.

### Success criteria (gate — soft)

- Perf: on `full_case_user_input`, `api.write.total_s` p95 drops by ≥10% at depth=64 vs depth=32.
- Bound: daemon RSS at end-of-run grows by ≤25% vs depth=32.
- Behavior: all 7 verification harness assertions pass.

### Decision use

If H3 alone hits the success criterion, it's a near-zero-cost win. If it doesn't, drop it — it's a sensitivity probe, not a solution.

---

## Comparison matrix (what gets filled in after the experiments)

| Variant | `write_file` p95 (ms) | `edit_file` p95 (ms) | `commit_resume_wait_s` p95 (ms) | `auto_squash.total_s` p95 (ms) | `auto_squash.raced` count | depth_before max | RSS Δ | Behavior gate | Verdict |
|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| **sync_baseline (today)** | 3,156.6 | 3,100.2 | 2,653.3 | 2,675.5 | 60 | 44 | — | PASS | reference |
| H1 coalesced | ? | ? | ? | ? | ? | ? | ? | ? | ? |
| H2 async | ? | ? | ? | ? | ? | ? | ? | ? | ? |
| H3 depth=64 | ? | ? | ? | ? | ? | ? | ? | ? | ? |
| H3 depth=128 | ? | ? | ? | ? | ? | ? | ? | ? | ? |
| **H1 + H3=64** | ? | ? | ? | ? | ? | ? | ? | ? | ? |
| **H2 + H3=64** | ? | ? | ? | ? | ? | ? | ? | ? | ? |

All numbers come from `auto_squash_compare.py` aggregation. Gate column is the verification-harness 7-invariant pass.

## Execution order (recommended)

1. **H3 first** (one constant; highest information / cost ratio). If it alone hits the bar, freeze and ship.
2. **H1 next** (lower-risk scheduler change). Coalesces cleanly with H3.
3. **H2 last** (largest behavior surface; needs the contract doc first).

Run each candidate in isolation before combining, so we don't entangle a behavior change with a scheduler change in the same delta.

## Cross-cutting requirements (apply to all three)

- Default behavior MUST stay synchronous-with-depth-32 until promotion is approved per scenario.
- Each candidate ships with **its own flag** (`EOS_OCC_SQUASH_MODE`, `EOS_OCC_AUTO_SQUASH_MAX_DEPTH`) so production stays on a known path.
- The 6 unit-test files listed in the verification plan §"Required Tests Already In Place" must pass under flag-off in every PR.
- The 5-scenario live harness must run once per candidate (and once per combination), with metrics captured under `.omc/perf/<variant>/<date>/`.
- Promotion to default takes its own PR with its own gate run.

## Out of scope (deliberately)

- Restructuring the commit serialization path — `commit_queue_wait_s` and `commit_worker_s` are already fast (≤6 ms p95 in baseline); not the bottleneck.
- Reducing snapshot lease cost — `api.{edit,write}.snapshot_read_s` is ≤17 ms p95; not the bottleneck.
- Reducing OCC prepare cost — `occ.prepare.total_s` p95 is ≤185 ms even on the hotspot; not the bottleneck.
- Changing default `AUTO_SQUASH_MAX_DEPTH` — only flag-controlled probes; the default stays at 32 unless H3 alone passes the gate.

## Open questions

1. Workspace lifetime vs squash worker lifetime in async mode: do we tie the worker to the sandbox lifecycle or to the OccService instance? (Probably the OccService — leases for the workspace already pivot on it.)
2. Is `raced=1` always wasted work, or does it sometimes successfully squash with a different base manifest? Confirm by inspecting the existing `raced` code path before claiming H1's expected `raced→0` outcome.
3. Memory ceiling for H3: at depth=128, what's the worst-case manifest size on a real workload? Probably worth a one-off measurement on `full_case_user_input` before running the full sweep.
4. Promotion order: if H1 and H2 both pass independently, do we ship H1 as default and keep H2 as opt-in, or vice versa? (Plan recommends H1 default, H2 opt-in until production telemetry confirms async failure surface is acceptable.)

## Acceptance for this plan document

- All three hypotheses are stated as testable predictions with numeric thresholds.
- Each has its own implementation surface, probe steps, and gate.
- The aggregator and harness referenced here already exist; no infrastructure work is required to start.
- A reviewer can read the comparison matrix as the deliverable: filling it in is the experiment.
