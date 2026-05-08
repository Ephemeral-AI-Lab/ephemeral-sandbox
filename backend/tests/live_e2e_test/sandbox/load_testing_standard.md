# Load Testing Standard — Live E2E Suite

Realizes §6 of `.omc/plans/per-call-snapshot-layer-stack-migration/live-e2e-test-suite-plan.md`.

## Profiles

Defined in `_harness/load_profiles.py`. Each `LoadProfile` is a frozen
dataclass; tests reference them by name.

| Profile     | Shells/s | Edits/s | Duration | Overlap | Gitignored | p99 budget | Drift | Emerg.-depth |
|-------------|---------:|--------:|---------:|--------:|-----------:|-----------:|------:|-------------:|
| `smoke`     |        2 |       4 |     30 s |    25 % |       40 % |     500 ms |     0 |            0 |
| `sustained` |        8 |      16 |     60 s |    50 % |       40 % |   1 000 ms |     0 |            0 |
| `burst`     |       30 |      60 |     20 s |    50 % |       40 % |   2 500 ms |     0 |            0 |
| `soak`      |        4 |       8 |     15 m |    35 % |       40 % |   1 200 ms |     0 |            0 |

`burst.max_emergency_depth_events` is held at 0 to match E5's pass bar
(plan §8 question 5; resolved here in favour of the stricter reading).

## Pass bars (apply to all profiles unless overridden)

- **Correctness** (mandatory): zero drift; every accepted write visible
  in the final merged view; every rejected write absent. Driven by
  `assertions.assert_accepts_visible_rejects_invisible` (lands with the
  integrated suite).
- **Latency telemetry**: integrated profiles record both host wall p99 and
  in-runtime p99. The original profile budget is evaluated against runtime p99
  and emitted as `runtime_budget_met`; host wall p99 is emitted separately as
  `wall_budget_met` because Daytona/provider dispatch varies independently of
  the in-sandbox runtime path.
- **Depth**: stack depth stays in `[SQUASH_TARGET-1, EMERGENCY_DEPTH-5]`
  except in `burst`, which may touch `EMERGENCY_DEPTH` ≤ 0 times.
- **Squash**: coalesce ratio ≤ 20 layers/s under `sustained` or `burst`.
- **Lease budget**: zero forced kills in `smoke` / `sustained` / `soak`;
  `burst` kills permitted only if `MAX_LEASE_AGE` is overridden in the
  profile.
- **Telemetry**: `manifest_lag` and `shell_age_seconds` present on every
  committed result; histogram emitted to the run JSONL.

## Telemetry contract

Every load run emits one JSONL record per call to
`.omc/results/live-e2e-<profile>-<utc>.jsonl`, matching the schema in
plan §6 (mirrors the existing `stack-overlay-live-*.jsonl` shape).

Current Phase 4 integrated artifacts use
`.omc/results/live-e2e-integrated-<profile>-<utc>.jsonl`. Each row includes
`wall_ms`, `runtime_ms`, `changed_paths`, `conflict_reason`, and the complete
public-tool timing map. Shell fan-out rows are produced by independent
`sandbox.api.shell` calls launched concurrently under
`gather_with_barrier`; each row carries the normal `api.shell.*` timing keys.

## Metric terminology

- `batch_wall_ms`: elapsed host wall time for one concurrency group, measured
  from releasing the shared barrier until every call in the group completes.
  For example, at concurrency 20 it measures the time for all 20 independent
  public API calls to finish.
- `per_call_p50_ms` / `per_call_p99_ms` / `per_call_max_ms`: percentiles over
  the individual API-call wall times inside that same group. In a barrier
  launched group, `batch_wall_ms` is normally close to the slowest individual
  call because the group ends when the slowest call ends.
- `throughput_ops_s`: completed operations divided by `batch_wall_ms` in
  seconds. For example, 20 calls in 2.5 seconds is 8 ops/s.
- `parallel_factor`: serial-equivalent time divided by `batch_wall_ms`, where
  serial-equivalent time is the concurrency-1 baseline multiplied by the number
  of calls in the group.
- `parallel_efficiency`: `parallel_factor / concurrency`; this shows how much
  of ideal linear scaling remains at that concurrency level.

## Subsystem Load Profiles

The native subsystem probes run inside the sandbox runtime bundle and are
bounded live tests, not the P2 stress ramp. They are defined as
`SubsystemLoadProfile` rows:

| Profile | Suite | Operation | Count | Concurrency | Budget |
|---|---|---|---:|---:|---:|
| `overlay_runner_load` | `overlay` | `runner.run_snapshot` | 20 | 20 | 1 000 ms p99 diagnostic |
| `layer_stack_load` | `layer_stack` | `manifest.append+publisher.publish` | 128 | 32 | 50 ms publish p99 |
| `occ_load` | `occ` | `orchestrator.commit` | 80 | 16 | 500 ms p99 |

## Drift definition under load

Realtime + replay (plan §8 q4):
1. **Realtime**: every commit asserts `assert_no_torn_reads(captures)`
   in-flight.
2. **Replay**: after the run, replay every captured upperdir against
   the final manifest and confirm accept/reject decisions match.

Both checks must pass for the run to count toward the promotion window.

## Promotion criteria

A profile is "passing" only when the last three runs on the same
image+kernel meet every bar. One failing run = re-run; two-of-three
failing = red, blocks the migration cutover step (Phase 06).
