# Phase 7 — RPC transport probe + persistent overlay supervisor

**Estimated effort:** 0.5d probes + 1.5–3d implementation (depends on which probe path lands)
**Risk profile:** MEDIUM — probes are bounded; supervisor-fork has well-defined failure modes; transport changes have larger blast radius if track 7.1(a) is taken
**Status:** Drafted; **probe-gated** — Tasks 7.0.A and 7.0.B must complete before any implementation track is opened
**Blocks on:** Phase 6 daemon-local fold remains the production path; Phase 6 parity corpus stays the correctness gate

> **Background.** Phase 6 collapsed two outer daemon subprocess stages into
> one in-namespace process and dropped 10× warm-path `svc.cmd` p50 from
> 2.614s to 1.805s. The new latency budget has two remaining large
> buckets that are addressable without touching OCC semantics or
> overlayfs internals:
>
> 1. **Orchestrator ↔ daemon RPC gap (~0.51s at 10× p50).** End-to-end
>    `svc_cmd_10x_latency` p50 = 1.805s; daemon-side `overlay_stage_total`
>    p50 = 1.298s. The delta is the host-side `transport.exec` HTTP
>    roundtrip carrying one `svc.cmd` call.
> 2. **Per-call `unshare` + `python3 overlay_run.py` boot (~0.4s of the
>    1.29s daemon total).** The runner re-imports the runtime, parses
>    argv, and pays interpreter startup on every call.
>
> Phase 7 attacks both via probes-first, then implementation tracks
> conditional on probe outcomes.

## Goal

Reduce 10× warm-path `svc.cmd` p50 from **1.805s** (Phase 6 measured)
to **< 1.20s**. Modeled landing zone if both probes succeed:
**~1.00–1.10s**. Partial-success gate: < 1.55s.

This remains a **perf phase**, not a feature phase. The Phase 6 parity
corpus is the correctness gate. No change to OCC semantics, snapshot
ordering, overlayfs layout, or `result.json` envelope shape.

## What is and isn't in scope

**In scope.**
- Two parallel probes (Tasks 7.0.A, 7.0.B) before any code change.
- Up to two implementation tracks gated on probe outcomes:
  - **Task 7.1** — persistent ci_rpc path; one of three sub-tracks
    (a)/(b)/(c) chosen by the Daytona transport probe.
  - **Task 7.2** — persistent overlay supervisor with per-request
    fork + `os.unshare` (no setns gymnastics, no shared user-ns).
- Reuse of Phase 6 parity corpus, extended with supervisor-specific
  tests (mount-cleanup, fd-leak, crash-recovery).

**Out of scope (deferred to Phase 8 or later).**
- **Lazy / post-command snapshot.** Changes the OCC strict-base
  semantics — peer writes during the user command would leak into
  base. Out.
- **Narrowed (upperdir-walker-driven) snapshot.** Plausible but
  requires its own parity corpus across create/modify/delete ×
  tracked/untracked/gitignored × concurrent-peer-writes. Phase 8
  candidate.
- **libgit2 / Rust snapshot helper.** Removes per-call `git`
  fork/exec but adds a build dep; lower priority than transport
  + boot wins. Defer.
- **Native Rust/Go overlay runtime.** Larger latency win than
  supervisor-fork but multi-week investment. Defer.
- **Streaming `on_progress_line`.** Phase 4 contract preserved.
- **The 1× cold-call tax (~6.7s).** Eager-bootstrap follow-up
  territory; not a warm-path lever.
- **Replacing overlayfs.** Already rejected in Phase 6 Appendix A.

## Probes (Task 7.0 series)

Probes are independent and may run in parallel. Both must report
before either implementation track is opened.

### Task 7.0.A — Daytona transport probe

**Question.** Does Daytona expose a host-reachable channel into the
sandbox that bypasses `transport.exec` per-call HTTP overhead?

**Hypotheses.**
1. **(a)** Daytona supports forwarding a TCP port from a sandbox to
   the host with keepalive → orchestrator opens one HTTP/2 or
   websocket connection and reuses it for every `svc.cmd`.
2. **(b)** Daytona supports a long-lived streaming exec channel
   (persistent stdin/stdout pipe) → daemon binds an internal unix
   socket; one persistent `exec` proxies bytes between host and the
   unix socket.
3. **(c)** Neither (a) nor (b) is available → fall back to RPC
   batching at the orchestrator.

**Procedure.**
1. Read the Daytona SDK docs and `backend/src/sandbox/daytona/transport.py`
   for: port-forwarding, preview URLs, websocket exec, long-running
   exec, capability flags.
2. Spike (a): bind a TCP listener inside a Daytona sandbox; from the
   host, attempt to open a socket. Measure 100 sequential `ping`
   roundtrips at warm steady-state. Compare against `transport.exec`
   per-call overhead measured by sending `printf 1` 100×.
3. Spike (b): same as (2) but using whichever long-lived exec verb
   exists. Measure `read_line` latency per request.
4. Record per-channel: p50/p95 roundtrip, max concurrent connections,
   idle timeout, payload size limit, reconnect behavior.

**Decision rules.**
| Outcome | Implementation track |
|---|---|
| TCP port-forward p50 < 50ms | **7.1(a)** persistent HTTP/2 daemon socket. Modeled gap reduction: 0.51s → ~0.05–0.10s. |
| Streaming exec p50 < 50ms but TCP unavailable | **7.1(b)** persistent exec proxy. Same modeled win, slightly more Daytona-specific code. |
| Neither persistent channel viable | **7.1(c)** orchestrator-side batching. Amortizes the 0.51s over the agent's typical burst factor (3–5). |

**Deliverable.** A short markdown note (`phase-07-probe-A-report.md`)
with measured numbers per channel, the chosen track, and the recorded
edge cases (idle timeout, max payload).

### Task 7.0.B — Supervisor-fork feasibility probe

**Question.** Does a long-lived Python supervisor that `fork()`s and
calls `os.unshare(CLONE_NEWUSER|CLONE_NEWNS)` per request hold up
under load and crash scenarios?

**Why this shape, not a worker-in-namespace.** `os.unshare()`
mutates the calling thread's namespace and `setns()` back to the
parent user-ns is privileged in the parent's view. A persistent
worker that re-enters a fresh namespace each call would need
saved-fd setns gymnastics with extra capabilities. Fork-per-request
keeps the supervisor itself in the original namespace and only ever
mutates the throwaway child.

**Hypotheses.**
1. A forked child can call `os.unshare(CLONE_NEWUSER|CLONE_NEWNS)`
   from Python without re-execing — semantically equivalent to
   `unshare -Urm`.
2. Repeated fork+unshare+mount+exit cycles do not accumulate state
   in the supervisor (fd count, RSS, mount-table size, zombie
   count) over 1000 iterations.
3. A child that crashes (SIGKILL, OOM, segfault, Python exception)
   does NOT poison the supervisor; the next request still succeeds.

**Procedure.**

Standalone harness at
`backend/tests/test_sandbox/test_code_intelligence/test_supervisor_fork_probe.py`:

1. Spawn supervisor; loop 1000 fork-unshare-mount-userland-exit
   cycles. Sample fd count, RSS, `/proc/self/mountinfo` line count,
   `/proc/self/task/*` count of supervisor every 100 iters.
   **Pass criterion:** zero growth across all four metrics.
2. Inject a child that calls `os.kill(os.getpid(), 9)` after the
   unshare; measure recovery and supervisor health.
3. Inject a child that allocates 4GB; verify the OOM-killed child
   doesn't take the supervisor with it.
4. Compare per-request latency against today's
   `subprocess.run([unshare, -Urm, bash, -lc, python3 overlay_run.py …])`
   baseline. Record p50/p95 for each path under the same workload.

**Decision rules.**
| Outcome | Action |
|---|---|
| Zero growth across 1000 iters AND crash scenarios recover | proceed with **Task 7.2 supervisor-fork**. Modeled win: -0.3 to -0.4s/call. |
| State leaks but crash recovery works | implement supervisor with **scheduled respawn every 200 calls**. Modeled win: ~0.25s/call. |
| Supervisor cannot survive child crashes cleanly | **abandon supervisor-fork.** Defer Python-boot reduction to a Phase 8 native runtime. |

**Deliverable.** `phase-07-probe-B-report.md` with measured tables,
recovery timings, and the chosen Task 7.2 variant (or its
abandonment).

## Implementation tracks (probe-conditional)

### Task 7.1 — Persistent ci_rpc path

Open exactly one of (a)/(b)/(c) based on Task 7.0.A.

#### Track 7.1(a) — TCP port-forward + HTTP/2 (preferred)

| Artifact | File | Purpose |
|---|---|---|
| Daemon TCP bind | `backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py` | bind a TCP listener on a fixed loopback port in addition to the existing unix socket |
| Daytona port reservation | `backend/src/sandbox/code_intelligence/rpc/launcher.py` | request port-forward at sandbox bring-up; resolve host-visible URL |
| Persistent HTTP/2 client | `backend/src/sandbox/code_intelligence/rpc/client.py` | replace per-call `transport.exec` dispatch with a kept-alive httpx (or h2) client; one connection per sandbox lifetime |
| Reconnect / fallback | `backend/src/sandbox/code_intelligence/rpc/client.py` | reconnect on EPIPE; fall back to `transport.exec` if 3× consecutive reconnects fail within 1 minute |
| Live perf E2E | `backend/tests/test_e2e/test_live_ci_phase7_persistent_rpc.py` (new) | 1×/5×/10× `svc.cmd` against the dask fixture; assert 10× p50 < track-(a) target |

#### Track 7.1(b) — Streaming exec proxy

| Artifact | File | Purpose |
|---|---|---|
| Stream wrapper | `backend/src/sandbox/daytona/transport.py` | `exec_stream()` over Daytona's long-lived exec verb |
| Daemon stdio loop | `backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py` | newline-delimited JSON requests on stdin → responses on stdout, request_id-tagged |
| Persistent client | `backend/src/sandbox/code_intelligence/rpc/client.py` | one persistent stream; route responses by request_id |
| E2E | same as 7.1(a) | |

#### Track 7.1(c) — Orchestrator-side batching (fallback)

| Artifact | File | Purpose |
|---|---|---|
| Batch dispatch | `backend/src/sandbox/code_intelligence/rpc/client.py` | coalesce concurrent `svc.cmd` calls within a 5–20ms window into one `transport.exec` carrying N requests |
| Daemon batch handler | `backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py` | receive batch envelope; dispatch each item to `OverlayAuditor.execute` concurrently (existing semaphore still bounds concurrency) |
| E2E | sequence of 10 commands fired with 0/5/15ms gaps; assert amortized p50 |

### Task 7.2 — Persistent overlay supervisor

Conditional on Task 7.0.B confirming feasibility.

| Artifact | File | Purpose |
|---|---|---|
| Supervisor process | `backend/src/sandbox/code_intelligence/overlay/runtime/supervisor.py` (new) | long-lived Python process with `runtime/*` imports loaded; accepts requests on a unix socket |
| Request protocol | same | newline-delimited JSON: `{request_id, run_dir, snap, upper_size_mb, user_cmd_b64, stdin_b64, timeout}`; response `{request_id, exit_code, error?}`; result.json/diff.ndjson/stdout.bin under run_dir as today |
| Per-request fork+unshare | same | parent forks; child closes listener fd, calls `os.unshare(CLONE_NEWUSER\|CLONE_NEWNS)`, then `runtime.runner.main_in_namespace(args)`; child writes result.json, calls `os._exit(rc)` |
| Crash isolation | same | parent uses `os.waitpid(child, 0)` with timeout; on timeout `os.kill(child, SIGKILL)`; on supervisor-side exception (request parse, fork failure) re-raise to daemon → daemon respawns supervisor |
| Daemon wiring | `backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py` | start supervisor at daemon boot; one supervisor per daemon lifetime |
| Auditor branch | `backend/src/sandbox/code_intelligence/overlay/auditor.py` | new daemon-local-supervisor branch: send request to supervisor over unix socket instead of `subprocess.run([unshare, …])` |
| Runtime entry refactor | `backend/src/sandbox/code_intelligence/overlay/runtime/runner.py` | extract `main_in_namespace(args: RunnerArgs)` from `main(argv)`; today's argparse path becomes a thin wrapper for the legacy/standalone call site |
| FD hygiene | supervisor.py | all listener / per-request sockets opened with `O_CLOEXEC`; child explicitly closes the listener pre-unshare |
| Parity test | `backend/tests/test_sandbox/test_code_intelligence/test_supervisor_parity.py` (new) | reuse Phase 6 five-case corpus through the supervisor; assert byte-equal `SimpleNamespace` results vs Phase 6 baseline |
| Live perf E2E | `backend/tests/test_e2e/test_live_ci_phase7_supervisor.py` (new) | 1×/5×/10× `svc.cmd`; assert 10× p50 < target; structural assertion that supervisor PID stays constant across all 16 calls |

## Combined targets

| Track | Modeled 10× p50 | Notes |
|---|---:|---|
| Phase 6 baseline (measured) | 1.805s | reference |
| 7.1(a) only | ~1.30s | -0.5s gap; daemon `total` unchanged |
| 7.1(c) only | ~1.55–1.65s | amortizes gap by typical agent burst factor |
| 7.2 only | ~1.50s | -0.3s python boot inside unshare |
| **7.1(a) + 7.2** | **~1.00–1.10s** | both buckets hit; primary target |
| 7.1(c) + 7.2 | ~1.20–1.30s | fallback combo if 7.1(a)/(b) unavailable |

**Ship gate.** Phase 7 ships if measured 10× warm-path `svc.cmd` p50
< **1.20s**. Partial gate: ship a single track if it lands < 1.55s
on its own and the other track is blocked or deferred.

## Risks

1. **Persistent connection idle timeouts (track 7.1(a)/(b)).**
   Daytona may kill an idle forwarded port after N minutes.
   Mitigation: keepalive pings every 30s; reconnect on first error;
   fall back to `transport.exec` if reconnect fails ≥ 3× in 1 minute.
   Quantified in Task 7.0.A.
2. **Supervisor mount-table accumulation (Task 7.2).** Each forked
   child performs tmpfs+bind+overlay+bind mounts. Child inherits
   parent's mount namespace until `unshare(CLONE_NEWNS)`; the
   unshare must happen **before any mount call**. Task 7.0.B
   verifies this; if cleanup leaks any host-side mounts,
   supervisor-fork is unsafe and Task 7.2 is abandoned.
3. **Fd inheritance (Task 7.2).** Child must close the supervisor's
   listener and any per-request socket inherited from the parent
   before running user code. `O_CLOEXEC` on all parent-side sockets
   enforces this. Verified in Task 7.0.B fd-growth assertion.
4. **OCC parity regression.** No correctness change is intended in
   Phase 7. The Phase 6 parity corpus is reused unchanged; any
   failure is a blocker, not a tradeoff.
5. **Daytona SDK version skew (track 7.1(a)/(b)).** If the chosen
   channel relies on a feature added in a recent SDK release, pin
   minimum version and gate the path on capability detection.
6. **Supervisor warm-up on cold daemon restart.** Daemon-restart
   path needs to spawn the supervisor before accepting `svc.cmd`.
   Existing eager-bootstrap (Phase 5) handles daemon readiness;
   extend its readiness criterion to include supervisor liveness.

## What ships

| Artifact | File | Status after Phase 7 |
|---|---|---|
| Phase 7 probe reports | `phase-07-probe-A-report.md`, `phase-07-probe-B-report.md` | new, written before any implementation |
| Phase 7 implementation report | `phase-07-implementation-report.md` | new, written at landing |
| Persistent RPC path | one of `rpc/client.py`, `rpc/launcher.py`, `daytona/transport.py`, `in_sandbox/ci_daemon.py` | modified per chosen 7.1 track |
| Overlay supervisor | `overlay/runtime/supervisor.py` (new), `overlay/runtime/runner.py`, `overlay/auditor.py`, `in_sandbox/ci_daemon.py` | new + modified per Task 7.2 |
| Parity test | `test_sandbox/test_code_intelligence/test_supervisor_parity.py` | new (only if 7.2 lands) |
| Live perf E2Es | `test_e2e/test_live_ci_phase7_persistent_rpc.py`, `test_e2e/test_live_ci_phase7_supervisor.py` | new |
| Probe harness | `test_sandbox/test_code_intelligence/test_supervisor_fork_probe.py` | new (kept after probe; doubles as a regression guard) |

## Sequencing summary

1. Run **Task 7.0.A** and **Task 7.0.B** in parallel. Each writes its
   report under `docs/architecture/code-intelligence-in-sandbox-daemon/`.
2. Open **Task 7.1** sub-track determined by 7.0.A; open **Task 7.2**
   if 7.0.B confirms.
3. Land tracks independently. Either may ship alone if the other is
   blocked, provided the partial gate is met.
4. Final live E2E exercises both paths together; the implementation
   report records the measured 10× p50 against the **< 1.20s** gate.
