# Phase 7 Probe A - Daytona transport channel report

Companion to
[`phase-07-rpc-transport-and-supervisor-fork.md`](./phase-07-rpc-transport-and-supervisor-fork.md).
Records the live Daytona transport probes for Task 7.0.A.

---

## 1. Verdict

**Verdict: no persistent transport track ships from Probe A.**

The sandbox was healthy: Daytona command execution worked, the CI daemon
path worked, and the final live E2E passed after production stayed on the
Phase 6 `transport.exec` ci_rpc bridge. The failing part was the attempted
Phase 7 persistent transport, not sandbox availability.

| Candidate | Probe result | Decision |
|---|---:|---|
| 7.1(a) TCP / preview URL | In-sandbox localhost returned `200`; host preview URL returned only `502` | Reject for this local stack |
| 7.1(b) long-lived session stdin + log stream | Isolated p50 `20.970ms`, p95 `24.881ms` after startup | Opened implementation attempt, then rejected by live concurrency |
| 7.1(c) batching | Not implemented in this pass | Remains the next plausible transport track |

The isolated session stream result was good enough to try 7.1(b), but the
integrated `svc.cmd` live workload failed: 10x p50 regressed to
`5.377s`, with requests queued behind one session stdin/log stream. The
single-channel session bridge therefore does not satisfy the Phase 7
gate.

---

## 2. Live evidence

### 2.1 Baseline

Command:

```bash
uv run pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
```

Result:

```text
1 passed
timing json: backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T20-07-13Z.json
```

| distribution | p50 | p95 | samples |
|---|---:|---:|---:|
| `svc_cmd_10x_latency` | `1.794s` | `1.837s` | 10 |
| `svc_cmd_10x_rpc_call_total` | `1.794s` | `1.837s` | 10 |
| `svc_cmd_10x_overlay_stage_total` | `1.278s` | `1.298s` | 10 |

### 2.2 Basic Daytona exec overhead

Timing artifact:
`backend/tests/test_e2e/_timings/phase_7_probe_transport_supervisor_2026-05-02T20-13-07Z.json`

| probe | p50 | p95 | max | samples |
|---|---:|---:|---:|---:|
| `transport.exec` 100x `printf 1` | `77.289ms` | `83.423ms` | `88.608ms` | 100 |

The same probe confirmed the sandbox Python was reachable:
`Python 3.11.14`, `euid=1000`.

### 2.3 TCP / preview URL path

Timing artifact:
`backend/tests/test_e2e/_timings/phase_7_probe_preview_session_2026-05-02T20-21-48Z.json`

| check | result |
|---|---|
| In-sandbox localhost request | `200` |
| Host preview URL status set | `[502]` |
| Host preview `ok_count` | `0` |
| Preview host | `8765-598a72dc-dfb9-4fa2-aee0-e8c9055f5f35.proxy.localhost:4000` |

The server was reachable from inside the sandbox, so this was not a daemon
or sandbox-down issue. The host preview/proxy path was not viable in this
local Daytona stack.

### 2.4 Session stream path

Timing artifact:
`backend/tests/test_e2e/_timings/phase_7_probe_session_stream_2026-05-02T20-23-56Z.json`

| probe | p50 | p95 | max | samples |
|---|---:|---:|---:|---:|
| `execute_session_command` 30x `printf` | `55.284ms` | `57.303ms` | `57.595ms` | 30 |
| Long-lived stdin + log stream 30x ping | `20.970ms` | `24.881ms` | `4528.505ms` | 30 |

The `4528.505ms` max was the startup/log-stream outlier. Warm p50/p95 were
below the 50ms decision rule, so 7.1(b) was the only implementation track
opened.

---

## 3. Implementation attempt result

The attempted 7.1(b) persistent session bridge used one Daytona session
command as a stdin/log-stream proxy to the daemon Unix socket. A direct
debug run after fixing Daytona's `input.pipe` warmup race showed the
isolated mechanism worked:

| call | result | elapsed |
|---|---|---:|
| First session request | `ECHO:first` | `10.553s` startup |
| Second session request | `ECHO:second` | `0.021s` |
| Forced exec bridge | `ECHO:exec` | `0.191s` |

The integrated live E2E then failed:

```bash
uv run pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
```

Timing artifact:
`backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T20-33-41Z.json`

| distribution | p50 | p95 | max | samples |
|---|---:|---:|---:|---:|
| `svc_cmd_10x_latency` | `5.377s` | `11.199s` | `11.199s` | 10 |
| `svc_cmd_10x_rpc_call_total` | `5.377s` | `11.199s` | `11.199s` | 10 |
| `svc_cmd_10x_overlay_stage_total` | `1.143s` | `1.169s` | `1.169s` | 10 |

The daemon-side overlay work stayed near Phase 6, while RPC total exploded.
That isolates the regression to the persistent session transport. The root
cause is head-of-line blocking and stream instability: concurrent `svc.cmd`
RPCs are serialized through one session stdin channel and correlated back
through one websocket log stream.

---

## 4. Final production verification

After rejecting the persistent session bridge and keeping production on the
Phase 6 exec bridge, the same live E2E passed:

```bash
uv run pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
# 1 passed in 69.78s
```

Timing artifact:
`backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T20-36-34Z.json`

| distribution | p50 | p95 | samples |
|---|---:|---:|---:|
| `svc_cmd_10x_latency` | `1.926s` | `2.442s` | 10 |
| `svc_cmd_10x_rpc_call_total` | `1.926s` | `2.442s` | 10 |
| `svc_cmd_10x_overlay_stage_total` | `1.352s` | `1.416s` | 10 |

The test gate is `svc_cmd_10x_latency.p50 < 2.000s`; the verified value was
`1.926s`.

---

## 5. Follow-up

Do not reopen the single-session stdin/log-stream bridge as-is. The next
transport attempt should be either:

1. **7.1(c) orchestrator-side batching**, so one `transport.exec` carries
   the burst instead of pretending a single stream is concurrent; or
2. A genuinely multiplexed Daytona channel with independent request slots
   and an integrated 10x live E2E gate before default enablement.
