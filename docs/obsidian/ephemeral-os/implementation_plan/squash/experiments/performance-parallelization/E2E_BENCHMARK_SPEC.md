# Live E2E Benchmark Spec — Squash Remount-Sweep (performance + correctness)

Status: highest-leverage subset **implemented + measured** (see `W-tuning.md`);
the remainder (straggler/fault/crash injection, RSS sampler, full N×W×M×B
nightly matrix) stays proposed. Extends the existing squash live-Docker suite
(`cli-operation-e2e-live-test/manager/management/squash/`) into a fuller
benchmark that jointly proves the remount-sweep parallelization (commit
`fab1c01b9`) **scales within budget** and is **outcome-preserving** under
concurrency, fault injection, and scale.

**Landed (`bench` tier, `test_squash_bench.py`):** the `M`/`B` scenario knobs
(`SQUASH_MIGRATE_RATIO`/`SQUASH_BLOCK_COUNT` via `_scenario_ab`, measured `M` from
harvested dispositions, exact `B`), the A/B driver + comparator
(`scripts/ab_driver.py`, `ab_compare.py`, `loadcombo_ab.py`), the `analyze_spans.py`
p95/aggregate extension, the `config/bench.yml` raised-log-cap config, and the
`sweep_width`/`swept` squash-span attrs. **CORR-AB-EQUIV PASS** (`W=1` vs `W=cores`)
on `AB-EQUIV` and LOAD-COMBO `N=200`. **PERF-WIDTH** curve done → **default `W = 4`**
(fixed constant `DEFAULT_REMOUNT_SWEEP_WIDTH`, the measured `sweep_wall` knee; `N=200`,
`M=1.0`: `951→368 ms`, 2.58×; no gain past `W=4`, per-migrated p95 `10→45 ms` from
oversubscription). Constant, not `available_parallelism()`, for a deterministic shipped
width; `EOS_REMOUNT_SWEEP_WIDTH` overrides per host. Not promoted to config
(`W-tuning.md`).

Grounding: attribution + before/after in
`experiments/performance-parallelization/perf-20260703-052525/{RESULTS,DESIGN}.md`.
Reuses the tooling already built: the `SQUASH_HARVEST_OBS` teardown harvest hook,
`scripts/analyze_spans.py` (overlap factor + per-disposition distribution),
`scripts/run_combo.sh`, and the `EOS_REMOUNT_SWEEP_WIDTH` width knob.

## 1. Goals & non-goals

Goals:
1. **Performance**: quantify sweep wall time, overlap factor, per-session cost,
   and end-to-end `T_squash` across a controlled matrix of session count `N`,
   migration count `M`, block count `B`, and sweep width `W`; confirm bounded
   budgets and near-linear overlap up to the core count.
2. **Correctness at scale/under concurrency**: prove the parallel sweep produces
   the same outcomes as the serial sweep (A/B equivalence), preserves every
   invariant, and classifies every disposition correctly while commands run.
3. **No RAM-for-speed**: assert peak daemon RSS growth during the sweep is
   `O(W)`, independent of `N`.

Non-goals: micro-benchmarking flatten internals (block build is 6–10 % of the op
— see RESULTS); kernel/overlay behavior (covered by MED/HRD tiers).

## 2. Design principles

- **A/B in one binary.** `EOS_REMOUNT_SWEEP_WIDTH=1` is the serial control and
  `W>1` the parallel arm — same daemon, same workload, so any delta is the sweep
  algorithm alone. Every performance case runs both arms; every correctness case
  asserts the two arms are equivalent.
- **Span-level truth.** Assertions read harvested `observability.ndjson`, not just
  harness wall time: `sweep_wall`, `sweep_serial_sum`, `overlap_factor`,
  per-disposition `dur_ms` distributions. Raise `observability.max_file_bytes`
  for the run so high-`N` runs do not rotate away early invocations (the 8 MB
  default evicted spans at `N=200` — see RESULTS §"200 sessions").
- **Deterministic thresholds.** Every case has numeric pass/fail bounds derived
  from the measured model, expressed as bands (not point values) to tolerate
  noise; report median + p95 over `K` repeats.
- **Controlled `M`.** The dominant cost is *migrated* sessions, not total. Cases
  set `M` explicitly (how many sessions hold layers the squash replaces) rather
  than relying on incidental migration.

## 3. Parameters & knobs

| knob | meaning | values under test |
|---|---|---|
| `N` (`SQUASH_COMBO_SESSIONS`) | live sessions swept | 50, 100, 200, 400, 499 |
| `W` (`EOS_REMOUNT_SWEEP_WIDTH`) | sweep concurrency | 1 (control), 2, 4, 8, 16 |
| `M` (new: `SQUASH_MIGRATE_RATIO`) | fraction of `N` that must migrate | 0.25, 0.5, 1.0 |
| `B` (new: `SQUASH_BLOCK_COUNT`) | squashable blocks in the stack | 1, 8, many |
| `cores` | container CPUs (`nproc`) | environment (baseline 4) |
| `K` (new: `SQUASH_BENCH_REPEATS`) | repeats per case | ≥ 3 |
| `max_file_bytes` | daemon log rotation cap | raised (e.g. 256 MiB) for the run |

## 4. Measurement kit

Captured per squash invocation (from harvested spans via `analyze_spans.py`,
extended to emit p50/p95):

- `T_squash` — harness wall (CLI→gateway→daemon round trip), max across invocations.
- `sweep_wall_ms` — `max(end) − min(start)` over `workspace_session.remount` spans.
- `sweep_serial_sum_ms` — Σ remount `dur_ms`.
- `overlap_factor` — `sweep_serial_sum / sweep_wall` (≈ effective concurrency).
- per-disposition distribution — n, p50, p95, max of `dur_ms` for
  Identity / Migrated / Leased / Faulty.
- `non_sweep_ms` — `parent − sweep_wall` (open+plan+build+commit).
- `T_http_disconnect` — max HTTP client silence (existing helper).
- `peak_rss_delta` (new) — daemon RSS sampled at ≥50 Hz during the sweep, minus
  pre-sweep RSS; the O(W) memory check.
- space/teardown — layer-dir count before/after, staging empty, remount residue,
  orphan workdir count (existing axes).

## 5. Performance matrix

| case | fixed | swept axis | assertion (parallel arm) |
|---|---|---|---|
| **PERF-SCALE** | `W=cores`, `M=0.5` | `N ∈ {50,100,200,400}` | `sweep_wall(N)` grows ~`⌈M/W⌉`, i.e. sub-linearly in `N`; `T_squash` under budget at every `N` |
| **PERF-WIDTH** | `N=200`, `M=0.5` | `W ∈ {1,2,4,8,16}` | `sweep_wall` monotonically decreases to `W=cores` then plateaus; `overlap_factor ≥ 0.7·min(W,cores,M)` |
| **PERF-ALLMIGRATE** | `N=200`, `M=1.0`, `W=cores` | — | worst-case `M=N`; `T_squash` under budget; overlap ≥ target |
| **PERF-STRAGGLER** | `N=200`, one injected slow-freeze session | `W ∈ {1, cores}` | serial arm `T_squash` ≈ straggler-budget + rest; parallel arm ≈ straggler-budget + `⌈(M−1)/W⌉·t̄` (tail isolated, not summed) |
| **PERF-MULTIBLOCK** | `N=200`, `B=many` (deep chains), `W=cores` | `B` | joint flatten+sweep under budget; `non_sweep_ms` (flatten) stays a minority of the op |
| **PERF-499** | `N` per LOAD-499, `W=cores` | — | 499-layer near-cap squash under budget; no regression vs current LOAD-499-HTTP |

Speedup expectation (measured baseline, 4 cores): `PERF-WIDTH` at `W=4`,
`N=200`, `M≈0.35` gave `overlap≈3.9×`, `sweep_wall 1043→267 ms`. Cases assert the
overlap **band** `[0.7·min(W,cores,M), min(W,cores,M)]` rather than a point.
Measured tuning run (`N=200`, `M=1.0`, `K=3`; `W-tuning.md`): `overlap` at `W=4`
`= 3.92` (band top), `sweep_wall` knee at `W=cores` (`951→368 ms`), plateau past
cores with a rising per-migrated tail — hence default `W = cores`.

## 6. Correctness matrix (under parallelism)

| case | setup | assertion |
|---|---|---|
| **CORR-AB-EQUIV** | identical deterministic workload | **logically equivalent** outcome between `W=1` and `W=cores`: identical disposition multiset, identical set of surviving pre-squash layer ids, identical squashed-block count, identical final manifest layer count. Not byte-identical — squashed layer ids are nonce-named (`S{ver}-{nonce}`), so `manifest_root_hash` differs run-to-run. Parallelism changes timing only, never outcome |
| **CORR-DISPOSITIONS** | population engineered to yield all four classes concurrently (idle→Migrated, PTY/cwd-pinned→Leased, fd/mount-pinned→Leased, post-PONR kill→Faulty) | each session's reported disposition matches its setup; `blocked_reasons` attribution per block is correct (never-straddle whole-or-none) |
| **CORR-INVARIANTS** | any migrating run | manifest version monotonic +1; exactly one surviving layer per squashed block; staging empty; zero orphan `work-remount-*` dirs; lease refcount GC removes only unreferenced layers (no premature GC of a pinned source); substitution recording order deterministic |
| **CORR-GATESTORM** | commands + publishes admitted *during* the sweep (admission-gate storm at `W=cores`) | no lost/duplicated command completion; no session left in a bad finalize state; a command admitted mid-remount serializes behind that session's gate (never interleaves the switch) |
| **CORR-FAULT** | inject, mid-parallel-sweep: (a) runner death post-PONR→Faulty; (b) persist failure; (c) freeze timeout→Leased | correct per-session classification; a fault in one worker never contaminates another session's outcome; strict teardown clean; faulty sessions destroyed via the ordinary path |
| **CORR-CRASH** | daemon SIGKILL mid-sweep, then restart | boot reap-then-sweep leaves storage consistent; switched-but-unpersisted handles reaped via `scratch_dir` (remount-invariant); no layer leak, no double-mount |
| **CORR-IDEMPOTENCE** | squash, then squash again | second run is a clean no-op (no blocks, sweep all Identity) at every `W` |

**CORR-AB-EQUIV is the linchpin**: it turns "is the parallel sweep correct?" into a
mechanical diff of two runs of the same deterministic workload. It must be gating.

## 7. Pass/fail criteria (consolidated)

A case passes iff, over `K` repeats (report median + p95):

- **Budgets**: `T_squash ≤ CASE_SQUASH_BUDGET_MS`; `T_http_disconnect ≤ 1500 ms`
  (hard, in-stream); `T_e2e ≤ CASE_E2E_BUDGET_MS`.
- **Overlap** (parallel arm, `M ≥ W`): `overlap_factor ≥ 0.7·min(W, cores, M)`.
- **Speedup** (A/B): `sweep_wall(W=cores) ≤ sweep_wall(W=1) / (0.6·min(cores,M))`.
- **Per-session tail**: Migrated `dur_ms` p95 ≤ ceiling (band from baseline; regress
  guard, e.g. ≤ 3× the serial p50 to bound oversubscription tax).
- **Memory**: `peak_rss_delta ≤ C·W` for a small constant `C` (handle+frozen-set
  size), and **not** correlated with `N` (fit slope vs `N` ≈ 0). This is the
  no-RAM-for-speed gate.
- **Correctness**: AB-equiv logical equivalence (disposition multiset + surviving
  pre-squash layer-id set + block count + manifest layer count — not byte-identical,
  since layer ids are nonce-named); all invariant assertions hold; space axis
  (layer dirs shrink, staging empty); strict teardown (no residue, no orphan
  workdir, gates-map drained).

## 8. Harness work required

Existing (reuse): harvest hook (`SQUASH_HARVEST_OBS`), `analyze_spans.py`,
`run_combo.sh`, `EOS_REMOUNT_SWEEP_WIDTH`.

New:
1. **`SQUASH_MIGRATE_RATIO`, `SQUASH_BLOCK_COUNT`, `SQUASH_BENCH_REPEATS`** scenario
   knobs; a scenario builder that deterministically controls `M` and `B` (create
   `M` sessions holding the to-be-replaced layers, `N−M` holding only boundary/base
   layers so they resolve Identity).
2. **A/B driver**: run each perf/correctness case twice (`W=1`, `W=cores`) and
   diff; a small comparator over the two harvested span sets + the two final
   `observability layerstack` snapshots.
3. **Straggler injection**: a session with a task parked in uninterruptible sleep
   (or a controlled `SIGSTOP`-resistant D-state proxy) to exercise the freeze
   budget; assert tail isolation.
4. **Fault injection hooks (test-only, not in `src/`)**: runner-death,
   persist-failure, freeze-timeout — driven from the E2E via existing
   fault scenarios (MED-13/17, freeze) extended to fire *during* a parallel sweep.
5. **RSS sampler**: sample daemon RSS (`/proc/<pid>/status` inside the container,
   via `docker exec`) at ≥50 Hz across the squash call; record `peak_rss_delta`.
6. **`analyze_spans.py` extensions**: p50/p95 per disposition; emit a machine
   verdict JSON with the §7 thresholds so CI can gate.
7. **Config**: raise `observability.max_file_bytes` for benchmark runs (via a
   benchmark config YAML passed through `SANDBOX_GATEWAY_CONFIG_YAML`) to prevent
   span rotation at high `N`.

## 9. Complexity / scaling expectations (asserted, not assumed)

Let `M` = migrated sessions, `W` = width, `cores` = container CPUs, `t̄` = mean
per-migration cost. The spec encodes these as bands:

- **Sweep wall**: `Θ(⌈M/W⌉·t̄)`. `PERF-WIDTH` asserts the `1/W` shape up to
  `W=cores`, then a plateau (no benefit past cores for the CPU-bound share).
- **Overlap**: `→ min(W, cores, M)`; asserted `≥ 0.7×` that.
- **End-to-end**: `T_squash = non_sweep + sweep_wall + round_trip`; as `sweep_wall`
  falls, `non_sweep` (flatten) and round-trip dominate — `PERF-MULTIBLOCK` guards
  that flatten stays a minority.
- **Memory**: `O(W)` working set, `O(1)` per session; `peak_rss_delta` slope vs
  `N` ≈ 0 is the gate. This is the empirical no-RAM-for-speed proof.
- **Tail**: a straggler adds `≤ freeze_budget` to **one** worker
  (`PERF-STRAGGLER`), vs `+freeze_budget` to the whole serial sweep.

## 10. How to run

```bash
export PATH="$PWD/bin:$PATH"
# raise the log cap + rebuild the daemon with the sweep code, restart gateway:
SANDBOX_GATEWAY_CONFIG_YAML=config/bench.yml bin/start-sandbox-docker-gateway --rebuild-binary

# A/B a case: serial control then parallel arm, harvest + analyze both.
SQUASH_HARVEST_OBS=1 EOS_REMOUNT_SWEEP_WIDTH=1  SQUASH_COMBO_SESSIONS=200 \
  pytest ...squash/test_squash_bench.py::...[PERF-WIDTH] -q
SQUASH_HARVEST_OBS=1 EOS_REMOUNT_SWEEP_WIDTH=4  SQUASH_COMBO_SESSIONS=200 \
  pytest ...squash/test_squash_bench.py::...[PERF-WIDTH] -q
python3 scripts/analyze_spans.py <case>/observability.ndjson* --json verdict.json
```

Artifact layout mirrors the current suite:
`test-reports/<run>/<case>/{verdict.json, combo-summary.json, observability.ndjson*,
ab-diff.json, rss-samples.json}`.

## 11. CI gating

- **Blocking** (every change touching the sweep/remount path): CORR-AB-EQUIV,
  CORR-INVARIANTS, PERF-WIDTH at `N=200` (overlap + budget), the memory gate, and
  the existing LOAD-COMBO/499/LARGE-HTTP.
- **Nightly** (full matrix): all `N`×`W`×`M`×`B`, straggler, fault, crash.
- **Baselines**: store the serial-arm (`W=1`) numbers per run as the regression
  reference; the parallel arm is judged against both the budget and its own
  serial control (A/B), so hardware drift cancels out.
