# Phase 2 — Daemon Process + Lifecycle: Implementation Report

Companion to
[`phase-02-daemon-lifecycle.md`](./phase-02-daemon-lifecycle.md).
Records the daemon lifecycle implementation, verification results, live
Daytona timings, and the implementation decisions that differ from the
draft plan.

---

## 1. Verdict

**Verdict: ships. Phase 2 daemon lifecycle is implemented and verified.**

The in-sandbox CI runtime now bundles and launches
`python -m sandbox.code_intelligence.daemon` as a long-lived asyncio
Unix-socket daemon under
`$HOME/.cache/eos-ci/<workspace_root_hash>/v1/daemon.sock`. The
orchestrator has a `DaemonBackend` using the Phase 2 Python socket shim,
one retry-after-respawn path, and a `DaemonLauncher` that uploads the
runtime bundle, starts the daemon, polls socket readiness, and shuts the
daemon down from `DaemonBackend.dispose()`.

The eager lifecycle hook changed from Phase 1's bundle+indexer behavior
to Phase 2's bundle+daemon behavior. The Phase 1 indexer still runs from
`DaemonBackend.ensure_initialized`; no mutation, overlay, LSP, or symbol
business logic moved into the daemon in this phase.

Live Daytona verification passed against the dask SWE image. The main
provider finding is that Daytona's `process.exec` may wait on detached
background descendants even for `setsid nohup ... & echo $!`; the launcher
therefore treats a spawn-command timeout as inconclusive and polls the
daemon socket before failing. This is why live spawn timings are higher
than the original 2s target while still functionally correct.

### 1.1 SLO reconciliation

The Phase 2 plan's DoD lists `daemon_spawn` < 2 s, `daemon_first_ping`
< 100 ms warm, and (via §2.6.A's test assertion) `create_sandbox_with_ci_
bootstrap` < 3 s. Live timings (§5) miss all three. This section
decomposes each into measured components — using the timing JSONs in
`backend/tests/test_e2e/_timings/phase_2_*.json` — and names which part
is structural debt versus a plan target that was infeasible from the
start.

**Why the previous reconciliation was wrong.** An earlier draft of this
section listed `daemon_spawn` as "5.8 – 25.3 s observed". That range was
constructed by treating four different timing-harness steps as if they
all measured spawn. They do not. None of the recorded steps isolate
pure-spawn cost; they all fold in either bundle upload, the first-ping
shim, or both:

| Step | File | What it actually measures |
|---|---|---|
| `clean_shutdown / initial_spawn` (5.822 s) | `phase_2_clean_shutdown_*.json` | First-ping shim only — daemon was already up from the prior `kill_and_respawn` test in the module-scoped fixture. **Not spawn.** |
| `dispose_no_orphan / spawn_daemon` (5.802 s) | `phase_2_dispose_no_orphan_*.json` | First-ping shim only — daemon already up from eager bootstrap inside the preceding `create_sandbox` step. **Not spawn.** |
| `kill_and_respawn / initial_spawn_and_ping` (25.343 s) | `phase_2_kill_and_respawn_*.json` | **Cold** bundle upload + spawn + first-ping shim. |
| `kill_and_respawn / daemon_respawn_via_call` (17.002 s) | same | Warm-bundle spawn + first-ping shim, after `kill -9`. |

Pure-spawn wall-time is therefore **upper-bounded by the launcher's own
defensive `timeout=5` cap on the spawn `transport.exec`**
(`launcher.py:450`), not measured directly. That cap is the plan-sanctioned
mitigation for the Daytona quirk in §6.3 — it is itself why pure-spawn
is structurally ≥ 5 s in this provider, regardless of how fast the daemon
process actually starts.

**Per-call exec baseline.** `pid_liveness_check` in
`phase_2_daemon_ready_after_create_*.json` is one `transport.exec`
running `cat … && kill -0 …`: **0.381 s**. Treat that as the floor for any
single orchestrator→sandbox round-trip; everything below subtracts it
where useful.

#### First ping — observed 5.862 s, target 100 ms (warm)

`DaemonCommandClient._send_frame_via_process_exec` (`daemon/client.py`)
base64-encodes each frame into a heredoc and runs `python3 - <<PY` via
`transport.exec` to reach the in-sandbox Unix socket. Each call pays:

- one `transport.exec` round-trip — ~0.4 s baseline,
- one cold `python3` interpreter startup inside the sandbox's conda
  environment — ~5 s observed (this is the dominant cost),
- the daemon's own `read_frame` / `handle_ping` / `encode_frame` —
  microseconds against a Unix socket.

The 100 ms target presumed the retired first-class transport verb, which the
plan explicitly pins to Phase 5: §2.4 ("Phase 5 replaces the entire shim
with the process.exec-backed daemon command") and the §503 MEDIUM risk callout
("Document the shim as Phase 2-4 only; Phase 5 measures the corrected process.exec-backed daemon path
verb against the shim"). **The Phase 2 implementation matches the plan's
design — the shim was always going to fail this SLO**, and the plan said
so. Disposition: **structural debt — closes when Phase 5 lands**.
Re-measure the SLO once `transport.exec` replaces the shim; gate Phase 5
acceptance on the shim-subtracted round-trip going under 100 ms.

#### Spawn — target 2 s, observed not directly measured

The launcher's spawn (`launcher.py:438-468`) issues
`setsid nohup python3 -m sandbox.code_intelligence.daemon … & echo $!`
under a `transport.exec(..., timeout=5)`. §6.3 records that Daytona's
`process.exec` blocks on detached background descendants even with
`setsid nohup ... </dev/null &`, so the launcher catches the timeout,
logs at DEBUG, and falls through to socket polling. The 5 s timeout is
not a measurement of daemon startup — it is the **defensive cap that
exists because of** the Daytona quirk.

Pure spawn cost can therefore be bounded but not measured from §5:

- Lower bound: the daemon must at minimum import `asyncio` + the
  Phase 2 modules, bind a Unix socket, write `daemon.pid`. Local
  unit-test runs (`test_daemon_server.py`) put this at < 0.5 s.
- Upper bound: 5 s (the launcher's `timeout=`) + socket-poll deadline
  10 s = 15 s before the launcher gives up.

The 2 s target conflicts with the plan's own §503 risk callout, which
acknowledges shim/spawn slowness as a HIGH risk. Disposition: **plan
target inconsistent with plan's own mitigation**. Action: in Phase 5,
when Phase 5 re-evaluates the process.exec bridge for spawn detection,
re-measure pure spawn against a daemon-log–derived "first listen"
timestamp.

#### Cold create — observed 31.4 s, target 3 s

Decomposing `phase_2_daemon_ready_after_create_*.json`'s
`create_sandbox_with_ci_bootstrap` against the live-test log lines (§5):

| Component | Observed | Phase-2 controllable? |
|---|---:|---|
| Daytona create + refresh | ~1 s | No |
| `ensure_git` probe ‖ bundle upload (parallel) | ~8 s | **Optimized — see below** |
| Daemon bootstrap after upload (warm marker + spawn + poll) | ~22 s | Partially |
| Of which: `is_alive` (home + pid lookup, 2 execs) | ~6 s | No — Daytona per-exec latency |
| Of which: warm `.bundle-hash` check | ~1 s | Yes (was ~8 s cold; now warm because parallel upload landed first) |
| Of which: spawn `timeout=5` cap | 5 s | Structural per §6.3 |
| Of which: socket poll | ~10 s | Reduced by the stable-loop fix; Phase 5 keeps the process.exec bridge |
| **Total** | **~31 s** | |

Daytona create plus `ensure_git` (alone) exceed the 3 s target on their
own, before any Phase 2 code runs. The 3 s budget assumed a warm bundle
and the unreachable 2 s spawn, neither of which holds on first contact
with a fresh sandbox. Disposition: **plan target was unreachable**.
Track this metric in warm-bundle mode in Phase 5 and re-target.

**Parallel-upload optimization.** Phase 1's bundle upload used to run
*inside* the eager bootstrap step, after `ensure_git` had completed.
Phase 2 splits that upload into a sibling phase
(`bootstrap_upload_runtime_bundle`) and submits it to a thread pool from
`SandboxService.create_sandbox` *before* `ensure_git` runs. The
subsequent eager bootstrap finds the bundle already in place via
`.bundle-hash` and only spawns the daemon. Wall-time savings on this
image are bounded by `min(ensure_git_time, upload_time)`:

| Run | `create_sandbox_with_ci_bootstrap` | ensure_git | upload | join wait |
|---|---:|---:|---:|---:|
| Pre-fix (2026-05-02T13:22:24Z) | 31.951 s | ~7 s | ~8 s (inline) | n/a |
| Post-fix sample 1 (T14:02:32Z) | 31.493 s | ~4 s | ~8 s (parallel) | ~4 s |
| Post-fix sample 2 (T14:05:43Z) | 31.439 s | ~4 s | ~8 s (parallel) | ~4 s |

On this dask SWE image the win is small (~0.5 s) because git was
pre-installed, making `ensure_git` the *short* pole. The structural
ceiling is `min(ensure_git, upload)`: in apt-install scenarios where
`ensure_git` runs ~30 s, the parallel block hides the full 8 s upload.
Bundle upload remains one-shot per sandbox via the `.bundle-hash`
marker, so this only affects first contact with a fresh sandbox.

#### What "ships" means here

Phase 2's contract is the daemon *lifecycle*: spawn detached, accept
framed msgpack, retry on death, clean up on shutdown. All four lifecycle
bullets pass live (§3, P2-001 through P2-005). The three SLO numbers
above are *debt that the plan documented in advance* — the §503 risk
callout explicitly named the shim cost as a Phase-2-through-4 problem
that Phase 5 resolves. What the plan understated was the wall-time hit
of stacking shim + chunked upload + Daytona's exec-proxy hold; this
report makes that explicit so Phase 5 can re-target each component
against the process.exec-backed daemon command.

---

## 2. File inventory

### Added

| Path | LoC | Purpose |
|---|---:|---|
| `backend/src/sandbox/code_intelligence/daemon/__main__.py` | 48 | `python -m sandbox.code_intelligence.daemon` entrypoint |
| `backend/src/sandbox/code_intelligence/daemon/server.py` | 270 | asyncio Unix-socket daemon, control dispatch, PID/socket cleanup |
| `backend/src/sandbox/code_intelligence/daemon/protocol.py` | 107 | 4-byte length-prefix + msgpack codec and schema validation |
| `backend/src/sandbox/code_intelligence/backends/` | 161 | `DaemonBackend`, Python socket shim, retry-after-respawn, typed daemon command errors |
| `backend/tests/test_sandbox/test_code_intelligence/test_daemon_server.py` | 207 | Protocol, dispatch, shutdown scheduling, local daemon lifecycle tests |
| `backend/tests/test_sandbox/test_code_intelligence/test_daemon_client_process_exec.py` | 146 | Daemon command success/error/retry tests with fake transport |
| `backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py` | 367 | Live Daytona spawn, ping, kill/respawn, shutdown, concurrency, dispose tests |
| `backend/tests/test_e2e/_timings/phase_2_*.json` | n/a | Passing live timing artifacts for daemon-ready, kill/respawn, shutdown, dispose |

### Modified

| Path | Change |
|---|---|
| `backend/src/sandbox/code_intelligence/daemon/launcher.py` | Adds `DaemonLauncher`, `DaemonUnavailable`, remote state-path helper, spawn/socket/shutdown logic; chunked uploader now decodes inline (no `.b64` staging file) |
| `backend/src/sandbox/lifecycle/workspace.py` | Eager bootstrap now ensures daemon readiness instead of running `ci_index`; adds `bootstrap_upload_runtime_bundle` for the parallel-upload path (§6.5) |
| `backend/src/sandbox/code_intelligence/backends/` | `DaemonBackend.ensure_initialized` ensures daemon lifecycle before Phase 1 indexing; `dispose()` shuts daemon down |
| `backend/src/sandbox/lifecycle/service.py` | Adds lifecycle progress logs around create/refresh/git/bootstrap; adds `_maybe_start_eager_ci_bundle_upload` / `_finish_eager_ci_bundle_upload` helpers, wires them into `create_sandbox` and `start_sandbox` (§6.5) |
| `backend/src/sandbox/lifecycle/proxy.py` | Adds `ensure_git` progress logs for live setup diagnosis |
| Existing Phase 0/1 tests | Updated expectations for daemon lifecycle, bundle contents, `DaemonBackend.dispose()`, and the inline-decode chunked upload |

### Deleted

None.

---

## 3. Per-story coverage map

| Story | Verdict | Evidence |
|---|---|---|
| **P2-001** Wire protocol | PASS | `protocol.py` implements `CI_PROTOCOL_VERSION=1`, `MAX_FRAME_BYTES=64MB`, msgpack length frames, `FrameError`, `SchemaError`, request/response dataclasses, and parse helpers. Unit tests cover round-trip, oversized header/body, bad version, and bad request schema. |
| **P2-002** Daemon server | PASS | `server.py` starts an asyncio Unix server, writes `daemon.pid`, binds `daemon.sock`, sets socket mode `0600`, dispatches `ping`/`shutdown`/`version`, handles stale dead PID/socket cleanup, rejects live PID startup, and removes PID/socket on shutdown. |
| **P2-003** Daemon entry | PASS | `__main__.py` parses `--workspace-root` and `--log-level`, returns 13 for `StorageUnavailable`, 11 for live stale daemon, 0 for normal shutdown. Bundle smoke test imports the daemon entry and dispatch table from extracted runtime. |
| **P2-004** daemon backend | PASS | `daemon/client.py` sends one frame through an inline Python Unix-socket shim over `transport.exec`, decodes the response frame, raises `DaemonCommandError` for error envelopes, and retries once through `DaemonLauncher.ensure_daemon()` after connection failure. |
| **P2-005** Launcher + eager lifecycle | PASS | `DaemonLauncher.ensure_daemon()` checks pid/socket, uploads runtime if needed, spawns via `setsid nohup`, polls socket readiness, and exposes shutdown. `bootstrap_in_sandbox_ci_runtime()` now calls this launcher from create/start/restart hooks. |
| **P2-006** Live E2E | PASS | `test_live_ci_phase2_daemon_lifecycle.py` passed as two live invocations: first daemon-ready test, then the remaining kill/respawn, clean shutdown, concurrent pings, and dispose tests. |
| **P2-007** Unit tests | PASS | New daemon-backend unit tests plus updated eager/bootstrap/backend/bundle tests pass in the sandbox suite. |
| **P2-008** Regression check | PASS | `uv run pytest backend/tests/test_sandbox -q` -> 478 passed. Ruff clean across changed source/tests. |

---

## 4. Verification

### Unit and regression

| Command | Result |
|---|---|
| `uv run pytest backend/tests/test_sandbox/test_code_intelligence/test_daemon_server.py backend/tests/test_sandbox/test_code_intelligence/test_daemon_client_process_exec.py -q` | **15 passed** |
| `uv run pytest backend/tests/test_sandbox/test_eager_ci_bootstrap.py backend/tests/test_sandbox/test_code_intelligence/test_runtime_bundle.py backend/tests/test_sandbox/test_code_intelligence/test_daemon_backend.py backend/tests/test_sandbox/test_code_intelligence/test_backends.py -q` | **59 passed** |
| `uv run pytest backend/tests/test_sandbox -q` | **478 passed** |
| `uv run ruff check backend/src/sandbox/code_intelligence backend/src/sandbox/lifecycle backend/tests/test_sandbox/test_code_intelligence backend/tests/test_sandbox/test_eager_ci_bootstrap.py backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py` | **All checks passed** |

### Live E2E

| Command | Result |
|---|---|
| `uv run pytest backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py::test_daemon_ready_after_create_sandbox -m live -v -s` | **1 passed in 43.74s** |
| `uv run pytest backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py -m live -k 'not daemon_ready_after_create' -v -s` | **4 passed, 1 deselected in 122.18s** |

---

## 5. Live timing summary

### Daemon ready after create — PASSED

Three samples against the dask SWE image; sample 1 is pre-fix, samples 2
and 3 are post-parallel-upload (§6.5):

```
                                  | sample 1 (pre)  sample 2  sample 3
create_sandbox_with_ci_bootstrap  | 31.951s         31.493s   31.439s
daemon_first_ping_no_retry        |  5.862s          5.811s    5.747s
pid_liveness_check                |  0.381s          0.313s    0.317s
--- TOTAL                         | 38.194s         37.617s   37.502s
```

Mid-flight logs from sample 2 show the bundle upload running concurrent
with `ensure_git`:

```
22:01:51 ensure_git starting
22:01:51 bundle upload (background) starting       -- parallel
22:01:55 ensure_git: git already available         -- 4s
22:01:59 bundle uploaded (109 KB, 5 chunks)        -- 8s (long pole)
22:01:59 background upload joined
22:02:05 CI daemon not alive                       -- is_alive 6s
22:02:05 spawning CI daemon
22:02:21 socket became ready                       -- spawn+poll 16s
```

`ensure_git` finished at 22:01:55 but the join blocked until 22:01:59
because the upload was the long pole; in this run the parallelization
hides the full 4 s of `ensure_git` behind the upload's 8 s. Pre-fix
sample's bundle upload ran *after* `ensure_git`, so the 7 s `ensure_git`
plus 8 s upload stacked. The post-fix wall-time win on this image is
small (`~0.5 s`) because `ensure_git` is short here; the parallel-block
ceiling is `min(ensure_git, upload)`.

### Kill -9 + respawn — PASSED

```
initial_spawn_and_ping:   25.343s
daemon_kill9:             0.647s
daemon_respawn_via_call:  17.002s
--- TOTAL: 42.992s ---
```

### Clean shutdown — PASSED

```
initial_spawn:            5.822s
shutdown_daemon_command:             0.439s
post_shutdown_settle:     0.501s
verify_pid_cleanup:       0.366s
verify_socket_cleanup:    0.330s
--- TOTAL: 7.459s ---
```

### Dispose cleanup — PASSED

```
create_sandbox:           24.591s
spawn_daemon:             5.802s
dispose_sandbox:          0.029s
--- TOTAL: 30.422s ---
```

`test_concurrent_pings` also passed. It is correctness-only and does not
write a timing JSON.

---

## 6. Implementation decisions

### 6.1 Eager bootstrap is daemon-only in Phase 2

Phase 1's eager hook ran the indexer synchronously. Phase 2 changes that
hook to upload the bundle and make the daemon reachable. Keeping the
indexer in `DaemonBackend.ensure_initialized` preserves the "no business
logic moves" rule while satisfying the daemon-ready lifecycle contract.

### 6.2 Python socket shim, not `socat` or `nc`

`DaemonBackend` uses an inline Python shim over `transport.exec` to connect
to the Unix socket and return a base64 response frame. This keeps Phase 2
independent of `socat`/`nc` availability. The shim is intentionally
the active Phase 5 path keeps it until batching or true provider-native persistent transport replaces the process.exec bridge.

### 6.3 Spawn timeout is treated as inconclusive on Daytona

Live Daytona showed that a detached background command can time out at
the exec API even when the daemon process did start. The launcher now
catches spawn-command exceptions, then polls the socket before failing.
This keeps correctness while documenting that Phase 2 timing is dominated
by the transport shim and provider process-exec behavior.

### 6.4 Live test setup logs are intentionally visible

The first live test originally looked hung because `create_sandbox()`
wrapped Daytona provisioning, refresh, git bootstrap, eager CI bootstrap,
and socket polling in one timing step. The live test now streams timestamped
logs from `sandbox.lifecycle.service`, `sandbox.lifecycle.proxy`,
`sandbox.lifecycle.workspace`, and `sandbox.code_intelligence.daemon.launcher`.
The production code uses normal `logger.info`; only the live test installs
a stdout handler.

### 6.5 Bundle upload runs concurrently with `ensure_git`

The eager bootstrap was originally a single sequential step:
`ensure_git` → `ensure_runtime_uploaded` → `spawn`. The first two steps
both depend only on the sandbox existing and have no shared state, so
running them serially leaves wall time on the table.

`SandboxService.create_sandbox` now submits the bundle upload via
`bootstrap_upload_runtime_bundle` to a single-shot
`ThreadPoolExecutor` *before* calling `sb.ensure_git()`. After
`ensure_git` returns, `_finish_eager_ci_bundle_upload` joins the
upload future. The subsequent `_maybe_run_eager_ci_bootstrap` finds
the bundle already in place via `.bundle-hash` and only runs the
spawn phase.

Best-effort by design: a failed background upload is logged at WARNING
level and swallowed, so the sequential bootstrap retries the upload
from scratch. This keeps the parallel path additive — never strictly
slower than the original.

The chunked uploader was also tightened in the same pass: each chunk
now pipes `printf | base64 -d >> bundle.tar.gz` directly into the
tarball, eliminating the previous `bundle.tar.gz.b64` staging file.
Safe because `_CHUNK_SIZE = 32 KB` is divisible by 4, so each chunk is
a self-contained 4-aligned base64 segment.

---

## 7. Hand-off to Phase 3

Phase 3 can assume:

- The runtime bundle can launch `python -m sandbox.code_intelligence.daemon`.
- The daemon owns `daemon.sock`, `daemon.pid`, and `daemon.log` under the
  workspace-hashed state dir.
- The orchestrator can issue framed msgpack daemon commands via `DaemonBackend._call_daemon_command`.
- Daemon crash between calls is covered by one retry-after-respawn path.
- `shutdown` removes the PID and socket files.

Phase 3 should add real code-intelligence verbs to the daemon dispatch
table without changing the process lifecycle contract again.
