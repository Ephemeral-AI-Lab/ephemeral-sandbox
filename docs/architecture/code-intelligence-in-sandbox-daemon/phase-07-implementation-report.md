# Phase 7 - RPC transport probe + supervisor fork: Implementation Report

Companion to
[`phase-07-rpc-transport-and-supervisor-fork.md`](./phase-07-rpc-transport-and-supervisor-fork.md).
Records the probes, attempted implementation, live verification, and ship
decision for Phase 7.

---

## 1. Verdict

**Verdict: does not ship.** Phase 7 produced live probe evidence and an
implementation attempt for the only probe-passing transport sub-track, but
the integrated live E2E rejected it. No production transport or supervisor
change is retained.

The sandbox was not down. Daytona command execution, CI daemon startup,
and the Phase 6 `svc.cmd` live e2e all worked. The failures were narrower:

1. Host preview/proxy for TCP returned `502` even while in-sandbox
   localhost returned `200`.
2. Python supervisor-fork is blocked because sandbox Python lacks
   `os.unshare`.
3. The long-lived session proxy passed isolated ping latency but failed
   the real concurrent `svc.cmd` workload through head-of-line blocking.

---

## 2. Scope completed

| Task | Result | Evidence |
|---|---|---|
| 7.0.A Daytona transport probe | Complete | [`phase-07-probe-A-report.md`](./phase-07-probe-A-report.md) |
| 7.0.B supervisor-fork probe | Complete | [`phase-07-probe-B-report.md`](./phase-07-probe-B-report.md) |
| 7.1(a) TCP/preview implementation | Rejected before implementation | Host preview returned only `502` |
| 7.1(b) session stream implementation | Attempted, then rejected | Live 10x p50 regressed to `5.377s` |
| 7.1(c) batching | Not implemented | Remains follow-up |
| 7.2 supervisor-fork | Rejected before implementation | `has_os_unshare=false` |
| Final live verification | Passed on unchanged Phase 6 exec bridge | 10x p50 `1.926s` under `< 2.000s` gate |

---

## 3. Implementation attempt

The attempted 7.1(b) shape was a persistent Daytona session command:

```text
orchestrator
  -> Daytona session stdin
  -> in-sandbox Python proxy
  -> CI daemon Unix socket
  -> session log stream response
```

The bridge used newline-delimited JSON envelopes, request IDs, base64
payloads, and a stdout marker to recover responses from the session log
stream. A Daytona startup race was also found: immediately sending input
after `execute_session_command(..., run_async=True)` can fail because
`input.pipe` does not exist yet. Retrying the input send fixed the
isolated bridge.

The direct debug result after that retry was:

| call | result | elapsed |
|---|---|---:|
| First persistent session request | `ECHO:first` | `10.553s` |
| Second persistent session request | `ECHO:second` | `0.021s` |
| Forced exec bridge request | `ECHO:exec` | `0.191s` |

That was enough to run the real live gate, but the live gate failed. The
attempted code was not retained because keeping an unused, failed
transport path would make the production transport harder to reason about.

---

## 4. Live performance results

### 4.1 Pre-implementation baseline

```bash
uv run pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
# 1 passed
```

Timing artifact:
`backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T20-07-13Z.json`

| distribution | p50 | p95 | samples |
|---|---:|---:|---:|
| `svc_cmd_10x_latency` | `1.794s` | `1.837s` | 10 |
| `svc_cmd_10x_rpc_call_total` | `1.794s` | `1.837s` | 10 |
| `svc_cmd_10x_overlay_stage_total` | `1.278s` | `1.298s` | 10 |

### 4.2 Failed 7.1(b) live gate

```bash
uv run pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
# failed: svc_cmd_10x_latency.p50 5.377s exceeded 2.000s
```

Timing artifact:
`backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T20-33-41Z.json`

| distribution | p50 | p95 | samples |
|---|---:|---:|---:|
| `svc_cmd_10x_latency` | `5.377s` | `11.199s` | 10 |
| `svc_cmd_10x_rpc_call_total` | `5.377s` | `11.199s` | 10 |
| `svc_cmd_10x_overlay_stage_total` | `1.143s` | `1.169s` | 10 |

The overlay stage remained healthy while RPC total regressed, so the
failure was in the session transport. The live run also reported the
session log stream closing unexpectedly after the burst.

### 4.3 Final verification after rejecting 7.1(b)

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

The final run passed the current Phase 6 gate (`10x p50 < 2.000s`) and
confirmed production did not keep the failed session bridge.

---

## 5. Verification commands

```bash
uv run pytest backend/tests/test_sandbox/test_daytona_transport.py \
  backend/tests/test_sandbox/test_code_intelligence/test_ci_rpc_client.py -q
# 29 passed
```

```bash
uv run ruff check backend/src/sandbox/daytona/transport.py \
  backend/tests/test_sandbox/test_daytona_transport.py
# All checks passed
```

```bash
uv run pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
# 1 passed in 69.78s
```

---

## 6. Ship decision

Phase 7 does not meet its ship gate:

| Gate | Target | Measured |
|---|---:|---:|
| Full Phase 7 ship | 10x p50 `< 1.200s` | Not achieved |
| Partial track ship | 10x p50 `< 1.550s` | Not achieved by 7.1(b) |
| No regression after rejection | Phase 6 gate `< 2.000s` | `1.926s`, pass |

The correct production state remains Phase 6:

```text
CiRpcClient -> DaytonaTransport.ci_rpc -> transport.exec inline Unix-socket bridge
```

No overlay supervisor is introduced, and no persistent session transport is
enabled.

---

## 7. Follow-up

1. Treat **7.1(c) batching** as the next transport candidate. It matches
   the observed failure mode because it amortizes `transport.exec` over a
   burst without relying on a single pseudo-interactive stream to behave as
   a concurrent RPC transport.
2. Keep supervisor-fork abandoned unless the sandbox exposes a verified
   namespace primitive. The Python `os.unshare` prerequisite is absent in
   the current live Daytona Python.
3. Preserve the Phase 6 live E2E as the guardrail for any future transport
   experiment. A probe-only ping result is not enough; the integrated 10x
   concurrent `svc.cmd` workload is the real decision point.
