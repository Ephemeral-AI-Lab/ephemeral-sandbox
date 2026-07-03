# Sweep-width (`W`) tuning — squash remount sweep

Follow-up to the shipped remount-sweep parallelization (commit `fab1c01b9`).
Answers: what should the default sweep width be, and does it belong in config?

**Result:** default `W = 4` — the measured `sweep_wall` knee, shipped as the constant
`squash.rs::DEFAULT_REMOUNT_SWEEP_WIDTH`. On the 4-core benchmark/target container
this equals `cores`; it is a fixed constant (not `available_parallelism()`) so the
shipped width is deterministic and pinned to the tuned knee. `EOS_REMOUNT_SWEEP_WIDTH`
overrides it. Tradeoff (accepted): a fixed 4 does not track core count, so a host with
materially fewer cores would oversubscribe (the tail-inflation regime below) and one
with many more would leave cores idle — retune via the env knob there. No config
promotion; the env knob covers override.

## Method

- Case `PERF-WIDTH` (`test_squash_bench.py`): a deterministic all-migrate topology
  — `N=200` idle sessions, `M=1.0` (every session pins the one squashable block),
  `B=1`. Clean signal: the sweep is 200 migrated remounts, nothing else.
- Driver `scripts/ab_driver.py` sweeps `W ∈ {1,2,4,8,16}`, `K=3` repeats each. `W`
  is the in-container daemon's `EOS_REMOUNT_SWEEP_WIDTH`; there is no host→container
  env passthrough, so each width rides in via `config/bench.yml`'s
  `manager.docker.container_env` and the gateway is restarted per width (no binary
  rebuild between arms). The daemon records the resolved width on the
  `layerstack.squash` span (`sweep_width` attr, added here), so every number below
  is confirmed against the width the daemon actually used, not the width requested.
- Metrics via `scripts/analyze_spans.py` (extended: p95 + `aggregate()`), pooled
  over the 3 repeats. `observability.max_file_bytes` raised to 256 MiB for the run
  so no spans rotate at `N=200`.
- Environment: sandbox container = **4 CPUs** (`nproc`), host = 14 (Docker Desktop).

## Curve (`N=200`, `M=1.0`, `K=3`, cores=4)

| `W` | overlap p50 | **sweep_wall p50 (ms)** | sweep_wall p95 | per-migrated p50 (ms) | per-migrated **p95 (ms)** | `T_squash` p50 (ms) | speedup vs W=1 |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1  | 1.00  | 951 | 1062 | 5  | 7  | 1005 | 1.00× |
| 2  | 1.97  | 535 | 540  | 5  | 7  | 587  | 1.78× |
| **4** | 3.92 | **368** | 384 | 7 | 10 | **417** | **2.58×** |
| 8  | 7.79  | 380 | 383  | 14 | 21 | 424 | 2.50× |
| 16 | 15.49 | 374 | 376  | 28 | 45 | 423 | 2.54× |

`non_sweep` (flatten + commit) held at ~18–20 ms across every width — the sweep is
the whole cost; flatten is a rounding error, as RESULTS predicted.

## Reading the curve — the knee is at `W = cores`

- **`sweep_wall` bottoms out at `W=4` (368 ms) and does not improve past it.** `W=8`
  (380 ms) and `W=16` (374 ms) are within noise of `W=4` — actually a hair *worse* at
  `W=8`. `T_squash` (harness wall incl. round trip) agrees: min at `W=4` (417 ms),
  flat/worse after. So the useful width is `cores`.
- **`overlap_factor` keeps climbing past cores (3.9 → 7.8 → 15.5) but that is an
  artifact, not a win.** `overlap = sweep_serial_sum / sweep_wall`; past cores the
  per-migrated *duration* balloons under oversubscription (p95 `10 → 21 → 45 ms`),
  which inflates `sweep_serial_sum` while `sweep_wall` stays pinned by the 4 cores.
  The rising overlap number is the tax showing up in the numerator, not throughput.
  **`sweep_wall` is the honest metric; `overlap` past cores is not.**
- **Oversubscription tax is real and steep past cores.** Per-migrated p95 doubles per
  width doubling beyond cores: `10 ms (W=4) → 21 ms (W=8) → 45 ms (W=16)`. At `W=16`
  each migrated session takes 6.4× its serial cost, bought nothing in wall time.
- **The wait-bound margin the handoff hypothesized did not materialize as wall gain.**
  The remount is partly wait-bound (subprocess `wait()`, freeze poll, fsync), but on
  a 4-core box those waits already overlap fully by `W=4`; adding threads past cores
  only lengthens each session under CPU contention.

Overlap up to cores is near-ideal: `1.97 / 3.92` at `W=2 / W=4` ≈ `0.98·W`, well above
the spec's `0.7·min(W,cores,M)` band.

## Decision

**Default `W = 4`, a fixed constant (`DEFAULT_REMOUNT_SWEEP_WIDTH`).**

- `4` is the measured knee on the 4-core target: it captures the full `2.58×`
  sweep-wall speedup at `N=200` and the isolated-straggler tail benefit, at the
  *lowest* per-session tax of any width that reaches the knee.
- A constant makes the shipped width deterministic and reproducible — the daemon
  sweeps 4-wide regardless of what `available_parallelism()` happens to report (which
  can over-count on shared/limited CPU hosts). On the 4-core sandbox container the two
  coincide; the constant removes the runtime dependency.
- The handoff's `min(2·cores, CAP)` option is rejected by the data: the knee is *at*
  4, not past it, and `W=8` was strictly worse on per-session tail for zero wall gain.
- Cost, stated plainly: a fixed `4` does not track cores. Below 4 cores it
  oversubscribes into the tail-inflation regime; well above 4 it leaves cores idle.
  Deployments off the 4-core profile should set `EOS_REMOUNT_SWEEP_WIDTH` (e.g. to
  their core count) after re-running this sweep.

## Config vs env — not promoted

`W` stays a constant default in `squash.rs::sweep_width()` with the
`EOS_REMOUNT_SWEEP_WIDTH` env override; it is **not** promoted into
`SandboxRuntimeConfig`/the operation layer. "Prefer less": the env knob already covers
per-host override and the serial control (`W=1`) that powers CORR-AB-EQUIV, so a config
field would add surface without new capability. If a future workload profile wants a
persisted non-4 default, promote then via `SandboxRuntimeConfig → operation` (not
`ObservabilityConfig`), keeping the env override.

## Production changes shipped

Two, both localized to `squash.rs`:
1. the `layerstack.squash` span records `sweep_width` and `swept`, so the width the
   daemon used is observable per invocation (this is what let the tuning verify W
   against ground truth, not the requested value);
2. `DEFAULT_REMOUNT_SWEEP_WIDTH = 4` replaces the `available_parallelism()` default.

Everything else (bench scenario, driver, comparator, analyzer extensions, bench config)
lives under the test harness / experiment dir.

## CORR-AB-EQUIV (linchpin, confirmed)

`W=1` (serial control) vs `W=cores` produce **logically identical** outcomes — proven
mechanically by `scripts/ab_compare.py` on the controlled `AB-EQUIV` case
(`wtuning/ab-ab-diff.json`, `pass=true`): identical disposition multiset
(`{Migrated:6, Identity:6}`), identical block count, identical final manifest layer
count, identical surviving-layer signature, identical space/teardown, and the arms
verified to actually differ in width (`1` vs `4`). Parallelism changes timing only.

Confirmed again on the noisy **LOAD-COMBO `N=200`** case (`scripts/loadcombo_ab.py`,
`wtuning/loadcombo-ab-diff.json`, `pass=true`): both arms pass every axis + teardown,
same disposition class set, and here even identical totals (`{Identity:201,
Migrated:212}`, 3 blocks, 103 replaced) despite that scenario's concurrent publishes.
`T_http_disconnect` `17 ms` (W=1) / `29 ms` (W=4) — both ~50× under the 1500 ms budget.
The parallel arm cut sweep wall `450→209 ms` (2.15×) with zero outcome change.

## Reproduce

```sh
export PATH="$PWD/bin:$PATH"
S=docs/obsidian/ephemeral-os/implementation_plan/squash/experiments/performance-parallelization/perf-20260703-052525/scripts
# one-time: package the daemon with the sweep + span attr and validate bench boot
SANDBOX_GATEWAY_CONFIG_YAML=<generated bench-W1.yml> bin/start-sandbox-docker-gateway --rebuild-binary
# CORR-AB-EQUIV (serial vs cores) -> wtuning/ab-ab-diff.json
python3 "$S/ab_driver.py" --case AB-EQUIV --label ab --widths 1,CORES --repeats 1
# PERF-WIDTH tuning curve -> wtuning/width-wtune.json
python3 "$S/ab_driver.py" --case PERF-WIDTH --label width --widths 1,2,4,8,16 --repeats 3 --sessions 200 --ratio 1.0
```
