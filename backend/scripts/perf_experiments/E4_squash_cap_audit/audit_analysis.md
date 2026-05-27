# E4 — Squash cap motivation audit

**Status:** Audit-data analysis (macOS-doable, no Docker needed for the analysis itself; controlled validation requires Docker).
**Motivation:** Plan §6 E1's value proposition depends on the assumption that `shell_pre_mount_squash` exists for a real reason — that mount latency degrades at high manifest depth. If depth doesn't correlate with mount latency, the 64-layer cap can simply be raised and the 204ms shell_pre_mount_squash p99 disappears with no mount-time penalty.

## Finding — strongly suggests the cap is over-conservative

Walking all 176 `.sweevo_runs/scenario_logs/*/*/performance_report.json` files and joining each `workspace.mount_s` measurement with the most-recent preceding `resource.layer_stack.manifest_depth` observation in the same event stream (5406 joined samples; 81 events had no preceding depth observation and are excluded):

| depth bucket | n     | p50 ms | p95 ms | p99 ms  | max ms  |
|--------------|------:|-------:|-------:|--------:|--------:|
| 1-7          |  713 | 23.36  | 108.20 | **178.24** | 192.14 |
| 8-15         |  703 | 22.51  | 79.08  |  95.24  | 120.22 |
| 16-31        |  905 | 23.13  | 78.80  |  94.74  | 136.84 |
| 32-63        | 1863 | 22.77  | 80.30  |  97.06  | 118.06 |
| **64-99**    | **1210** | **22.64** | **25.55** | **30.43** | **96.05** |
| 100-199      |   12 | 23.08  | 26.26  |  26.26  |  26.26 |

The **p99 mount latency is roughly 6× lower at depth 64-99 than at depth 1-7**, and p50 is essentially flat across the range. There is no evidence that manifest depth in the observed range (1-100) degrades mount performance. The 100-199 bucket is undersampled (n=12) but consistent with the trend.

## Interpretation

Three plausible explanations:
1. **The cap is empirical heuristic without a kernel-level cause** — chosen for cold-cache or storage-pressure reasons that aren't manifested in `workspace.mount_s`. Per the original commit (bc2927a01), motivation was "reduce high-concurrency shell mount pressure" — possibly addressing a different bottleneck than mount syscall latency.
2. **Selection bias**: depth>64 only happens in scenarios where mount is otherwise fast (steady-state long-running daemons), while depth<8 includes cold-start mounts that pay one-time overhead.
3. **The mount path is genuinely depth-insensitive** in the [1,200] range because the codebase already uses `fsmount(2)`/`move_mount(2)` (sandbox/overlay/kernel_mount.py:49-75) which accepts ≥200 layers without degradation.

Explanation (3) is the most likely given the kernel facts, but (2) cannot be ruled out from observational data alone.

## Recommended validation

Run a single Docker-backed scenario (`heavy_io_zoned_concurrent` or `full_case_user_input` — the worst-tailed scenarios per the original audit) twice:

- **Run A**: `EOS_SHELL_MOUNT_SQUASH_MAX_DEPTH=64` (current default).
- **Run B**: `EOS_SHELL_MOUNT_SQUASH_MAX_DEPTH=200`.

Compare:
- `workspace.mount_s` p99 — must not regress by more than 10%.
- `layer_stack.shell_pre_mount_squash.total_s` p99 — should drop by ~200ms (or to 0 if depth never crosses 200).
- `resource.layer_stack.manifest_depth` distribution — should now reach the new higher cap.

If Run B mount p99 stays ≤ Run A + 10%, the cap is safe to raise; ship the env override or change the default in `_shell_mount_squash_max_depth`.

## Why this matters

E1 (async squasher) attacks the 204ms hotspot by deferring the squash off the critical path. **If the squash can simply be skipped** (cap raised, never triggers), E1 is unnecessary. This is the cleaner fix:

- E1: complex multi-process work, capability-gated, ~3 days, daemon lifecycle changes.
- E4: one env var change, one validation run, ship config in a separate PR. Zero code complexity.

## Status

- Analysis: complete and macOS-doable (uses only the existing audit data).
- Validation: requires Docker — defer to a Linux session.
