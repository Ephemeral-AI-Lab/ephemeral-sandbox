---
platform: "macOS-26.4.1-arm64-arm-64bit"
kernel: "25.4.0"
python_version: "3.12.6"
docker_version: "Docker version 28.3.0, build 38b7060"
provider_image_tag: "n/a"
cpu_model: "Apple M3 Max"
wall_clock_at_start: "2026-05-27T18:10:31+0800"
page_cache_handling: "macOS-uncached"
experiment: "E2A-batch-window-sweep"
n_submitters: 8
commits_per_submitter: 10
iters_per_window: 30
warmup_per_window: 5
windows: "[0.0, 1e-05, 0.0001, 0.0005, 0.001, 0.002, 0.005, 0.01]"
production_p99_ms: 47.5
---

# E2/A — OCC batch-window sweep report

**Plan:** docs/plans/sandbox_perf_experiments_PLAN.md §6 E2/A.  
See README.md in this directory for design and threshold rationale.


## Verdict

**VERDICT: KILLED** — best alternative window (0s) reduces p99 by only **3.734ms** vs baseline (0.002s). Threshold of ≥50ms not met. Per plan §6, no E2/A integration ships from this run. Either E2/B (pipelined publisher) is required, or the 332-804ms tails are filesystem-stall artifacts not addressable from inside the batcher.


## Sweep results

| batch_window_s | n | median ms | p95 ms | p99 ms | max ms | Δp99 vs default ms |
|---:|---:|---:|---:|---:|---:|---:|
| 0 | 2400 | 0.419 | 26.457 | 27.242 | 27.835 | -3.734 |
| 1e-05 | 2400 | 0.473 | 27.250 | 27.695 | 28.896 | -3.280 |
| 0.0001 | 2400 | 0.465 | 26.282 | 27.692 | 31.654 | -3.283 |
| 0.0005 | 2400 | 0.476 | 27.180 | 29.192 | 62.306 | -1.783 |
| 0.001 | 2400 | 0.485 | 27.676 | 28.933 | 32.014 | -2.043 |
| 0.002 | 2400 | 0.465 | 28.731 | 30.976 | 31.173 | +0.000  ← **default** |
| 0.005 | 2400 | 0.470 | 33.201 | 36.650 | 47.742 | +5.675 |
| 0.01 | 2400 | 0.479 | 39.527 | 41.639 | 42.018 | +10.663 |

## Ceiling reasoning

Per `commit_queue.py:132-167`, the batch window adds at most one `time.sleep(batch_window_s)` per batch — i.e. ≤ batch_window_s overhead **per batch**, not per item. Even if every commit produced its own batch, the ceiling on what tuning can save from this 2ms window is **~2ms per batch**. Production p99 = 47.5ms; tail max up to 804ms. Therefore the batch window cannot be the load-bearing cause of the tail. If this bench shows a small-but-real reduction, ship-or-skip on whether the marginal gain justifies the operational change. If this bench shows no reduction, the prod tail is sourced elsewhere (publisher latency, filesystem stalls), and E2/B or different optimization paths are required.


## Methodology

- Setup: fresh `LayerStack` on local disk; `CommitTransaction` wraps it; one `CommitQueue` per swept batch_window_s value (drained + closed between conditions).
- Workload per iteration: N=8 threads synchronize on a `threading.Barrier`, then each thread submits 10 `PreparedChangeset` items targeting disjoint paths (DIRECT route, single WriteChange per group).
- Per-window: 5 warmup + 30 timed iters.
- Sample = per-commit `occ.serial.queue_wait_s` (TimingKey.COMMIT_QUEUE_WAIT). Stats are over the full sample set across iters: 30 × 8 × 10 = 2400 samples per window.
- Paths are unique per (iter, thread, commit) tuple so batches stay disjoint and the batcher's path-collision predicate never falsely defers an item.

