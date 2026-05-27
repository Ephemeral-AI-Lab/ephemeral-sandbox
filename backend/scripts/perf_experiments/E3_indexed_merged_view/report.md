---
platform: "macOS-26.4.1-arm64-arm-64bit"
kernel: "25.4.0"
python_version: "3.12.6"
docker_version: "Docker version 28.3.0, build 38b7060"
provider_image_tag: "n/a"
cpu_model: "Apple M3 Max"
wall_clock_at_start: "2026-05-27T18:02:47+0800"
page_cache_handling: "macOS-uncached"
experiment: "E3-indexed-merged-view"
depths: "[10, 50, 100, 200]"
iters_per_condition: 30
lookups_per_iter: 1000
files_per_layer: 50
---

# E3 — MergedView aggregate-index spike report

**Plan:** docs/plans/sandbox_perf_experiments_PLAN.md §6 E3.  
**See README.md** in this directory for design and threshold rationale.


## Verdict

**VERDICT: PROMOTED at the microbench level — does NOT subsume E1** — IndexedMergedView achieves ≤2× scaling (actual: 1.02×) while incremental index update stays within budget. The prototype works as designed.

**However, the chain assumption is empirically falsified.** Reading `backend/src/sandbox/ephemeral_workspace/pipeline.py:243-274`: `_run_shell_pre_mount_maintenance` docstring is *"Collapse deep manifests before shell enters the kernel mount path."* The squash exists to manage **shell-mount-time pressure**, not for `read_bytes` performance. The trigger fires on `depth_before > max_depth` (default 64) regardless of how fast the in-process read path is.

Note: the codebase already calls `fsmount(2)` / `move_mount(2)` directly via `sandbox/overlay/kernel_mount.py:49-75`, so the util-linux `mount(8)` 16-layer cap suggested by the earlier-recorded `overlay_depth_cap_root_cause` memory does **not** apply here — the mount(2) syscall accepts 200+ layers. The **64-layer cap motivation remains empirically unconfirmed in this audit** (commit `bc2927a01` says "reduce high-concurrency shell mount pressure" — observed empirically, not derived from a kernel limit). Knowing the real motivation is a precondition for deciding whether E1 (async squasher) is needed or whether the cap can simply be raised.

**Consequence per plan §6 decision tree:** E3 PROMOTED at the spike level **but does NOT subsume E1** — they attack different layers of the same hotspot. IndexedMergedView can ship for its own read-perf wins (175× faster absent lookups at L=200, 6× faster present lookups), but it does not delete the 204ms shell-pre-mount-squash tail. That requires either E1 (async squasher) or first establishing why the 64-layer cap exists and whether it can be relaxed.


## Load-bearing assumption (advisor-flagged)

E3 measures `read_bytes` latency, not squash latency. The claim that E3 subsumes E1 depends on **read perf being the dominant reason squash exists**. If squash is needed for the overlay-mount layer cap (util-linux 2.41 mount(8) limits at 16 layers; mount(2) syscall takes 199+, per [overlay_depth_cap_root_cause](memory)), then E3 passing does not eliminate the need for E1 — it only removes the read-perf justification. Integration PR for E3 must confirm the mount-time constraint is addressed separately (or accept that some bounded squash is still needed).


## Realism gate

- Baseline (`MergedView`) median scaling L=10 → L=200: **18.08×**

- Realism gate: required ≥5× (doc claims O(L)) — **PASS**


## Scaling table — workload A (absent-only, worst-case walk)

Per-lookup latency (mean of 1000-lookup batches), reported in **µs**:


| condition | L | n | median µs | p95 µs | p99 µs | max µs | median 95% CI |
|---|---:|---:|---:|---:|---:|---:|---|
| baseline (MergedView) | 10 | 30 | 17.98 | 18.78 | 18.83 | 18.84 | [17.86, 18.17] |
| treatment (IndexedMergedView) | 10 | 30 | 1.84 | 1.86 | 1.90 | 1.91 | [1.83, 1.84] |
| baseline (MergedView) | 50 | 30 | 82.40 | 84.64 | 84.71 | 84.74 | [81.91, 82.83] |
| treatment (IndexedMergedView) | 50 | 30 | 1.84 | 1.91 | 1.91 | 1.91 | [1.83, 1.85] |
| baseline (MergedView) | 100 | 30 | 162.39 | 164.68 | 165.86 | 166.19 | [161.79, 163.09] |
| treatment (IndexedMergedView) | 100 | 30 | 1.84 | 1.90 | 1.91 | 1.91 | [1.84, 1.85] |
| baseline (MergedView) | 200 | 30 | 325.08 | 332.55 | 335.52 | 336.53 | [324.10, 327.82] |
| treatment (IndexedMergedView) | 200 | 30 | 1.88 | 1.97 | 1.97 | 1.97 | [1.87, 1.89] |

## Scaling table — workload B (present paths, file-read dominated)

Per-lookup latency (mean of 1000-lookup batches), reported in **µs**:


| condition | L | n | median µs | p95 µs | p99 µs | max µs |
|---|---:|---:|---:|---:|---:|---:|
| baseline (MergedView) | 10 | 30 | 42.43 | 43.53 | 43.84 | 43.85 |
| treatment (IndexedMergedView) | 10 | 30 | 27.76 | 30.83 | 31.59 | 31.87 |
| baseline (MergedView) | 50 | 30 | 77.73 | 79.83 | 81.48 | 82.12 |
| treatment (IndexedMergedView) | 50 | 30 | 28.63 | 29.37 | 29.46 | 29.48 |
| baseline (MergedView) | 100 | 30 | 120.03 | 132.89 | 133.95 | 134.12 |
| treatment (IndexedMergedView) | 100 | 30 | 29.95 | 33.28 | 34.68 | 34.73 |
| baseline (MergedView) | 200 | 30 | 205.33 | 226.16 | 227.52 | 227.91 |
| treatment (IndexedMergedView) | 200 | 30 | 31.64 | 35.00 | 36.05 | 36.46 |

## Incremental publish + index update cost (at L=200)

| op | n | median ms | p95 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|---:|
| LayerStack.publish_changes | 30 | 9.624 | 11.146 | 24.868 | 30.446 |
| IndexedMergedView.add_layer | 30 | 2.7267 | 2.9440 | 3.0406 | 3.0738 |

Index update p99 = **3.0406ms** (15.20% of 20ms budget) — **PASS**


## Methodology

- Fixture: synthetic LayerStack on tmpfs/local disk. Each layer publishes 50 unique files under nested dirs (`dirA/subB/...`). Files are tiny (<50 bytes) so file-read overhead is comparable to hash-lookup overhead — emphasising the per-layer walk cost the index is hypothesised to eliminate.
- Workload A: 100% absent lookups → forces full O(L) walk in baseline; pure walk-cost signal.
- Workload B: present paths sampled uniformly across the manifest → average walk is L/2 layers; file-read overhead dominates.
- Iters/condition: 30 timed + 5 warmup. Each iter does 1000 lookups; sample = per-lookup mean for that iter. Stats are over the 30 per-iter means (bootstrap CI95, 1000 resamples).
- Caches kept warm between iters (production steady-state). LayerIndex cache is built once per view; we do not clear it between iters.

