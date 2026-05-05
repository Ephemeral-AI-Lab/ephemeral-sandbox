# Two-Server Architecture — overlay-server + occ-server

**Status:** draft (proposed)
**Author:** 2026-05-06
**Predecessor:** `.omc/plans/per-call-snapshot-layer-stack-migration/api-latency-reduction-plan.md` (Phases 1-4 landed)
**Related artifacts:**
- `backend/src/sandbox/runtime/daemon.py` (current single-process daemon)
- `backend/src/sandbox/runtime/api_handlers.py` (mixed overlay + OCC handlers)
- `backend/tests/live_e2e_test/sandbox/phase-04-latency-attribution-report.md`
  (post-Phase-4 measurements, including the 3-way A/B/C sweep that motivates this work)

## Why this exists

Phase 3 of the latency-reduction plan introduced a resident in-sandbox daemon
that handles every public sandbox API call. The post-implementation A/B/C
sweep showed an architectural problem the original plan didn't anticipate:

| c=16 p99 | daemon | daemon+pool | fork |
|---|---:|---:|---:|
| read wall | 709 ms | **668 ms** | 977 ms |
| write wall | 718 ms | **718 ms** | 1032 ms |
| edit wall | 870 ms | **759 ms** | 1120 ms |
| **shell wall** | **3144 ms** | **3079 ms** | **1478 ms** |

Daemon mode beats fork on read / write / edit by ~30 % (correct), but
**loses to fork on shell by ~100 %** because the daemon is a single Python
process running an asyncio event loop. Sixteen concurrent shell calls all
funnel through one GIL, one `RuntimeInvoker`, and one `OccSerialMerger`
worker thread. Fork mode gives 16 actual processes with 16 actual
GILs and beats the daemon on overlay-heavy verbs.

The current daemon is doing more than fork-per-call but less than a real
multi-process server. The right fix is to split it into two purpose-built
services with clear state ownership and per-service parallelism.

## Goal

Replace the single `sandbox.runtime.daemon` process with **two long-lived
in-sandbox servers**:

* **overlay-server** — stateless service. Handles `mount + exec + capture`
  for shell calls. Internally uses a small pool of pre-warmed child
  processes so concurrent shells run on multiple GILs. Closes the
  daemon-vs-fork shell-at-c16 gap.
* **occ-server** — single-instance service. Owns the `LayerStackManager`,
  `OccService`, gitignore oracle, and the `OccSerialMerger`'s worker
  thread. Single-writer by design (matches OCC's total-order semantics).
  Hosts read / write / edit / commit / capture-publish endpoints.

Both servers listen on AF_UNIX sockets inside the sandbox. The host still
talks to the sandbox through exactly one `process.exec` per public-API
call; a thin sandbox-side `sh` script picks the right socket based on the
op name.

## Architecture

```
┌─────── host ──────────────────────────────────────────────────────┐
│  sandbox.api.tool.{shell,write_file,edit_file,read_file,…}        │
│      │                                                             │
│      ▼  (one envelope, one process.exec)                           │
│  control/daemon/command.py — _call_runtime_server                  │
│      │                                                             │
│      └── exec_fn(sandbox_id, "thin_client.sh '<json>'") ───────┐   │
└────────────────────────────────────────────────────────────────┼───┘
                                                                 │
                                  ┌──────────────────────────────┼─── sandbox ──┐
                                  │ /tmp/eos/thin_client.sh      │              │
                                  │     reads $1, picks socket   │              │
                                  │     by op (overlay.* → ovr,  │              │
                                  │     api.*    → occ)          │              │
                                  │     pipes envelope → recvs   │              │
                                  │                              ▼              │
                                  │   ┌── overlay-server ──┐  ┌── occ-server ──┐│
                                  │   │ AF_UNIX:           │  │ AF_UNIX:        ││
                                  │   │ /tmp/eos/ovr.sock  │  │ /tmp/eos/occ.s  ││
                                  │   │                    │  │                 ││
                                  │   │ child_pool[N=8]    │  │ asyncio loop    ││
                                  │   │  ├ child-1 (GIL)   │  │  ├ prepare-pool ││
                                  │   │  ├ child-2 (GIL)   │  │  │  (Phase 3.x.1)│
                                  │   │  └ …               │  │  └ serial merger││
                                  │   │                    │  │     thread      ││
                                  │   │ stateless          │  │ owns layer-stack││
                                  │   └────────────────────┘  └─────────────────┘│
                                  │                              ▲              │
                                  │   (shell flow: client → ovr → occ ──────────┘│
                                  │    one round trip, two server hops inside)   │
                                  └──────────────────────────────────────────────┘
```

The daemon-as-router pattern. Each server owns one concern. Host stays
inside the provider adapter — `process.exec` remains the only host↔sandbox
transport; everything below that lives in the sandbox.

## Scope

### In scope

* Build `sandbox/runtime/overlay_server.py` (new) — AF_UNIX listener,
  child-process pool for `mount + exec + capture`, returns the captured
  changes as a JSON envelope.
* Build `sandbox/runtime/occ_server.py` (new) — AF_UNIX listener, hosts
  the OCC handlers currently in `api_handlers.py` (plus the read/metric
  ops), folds in the existing prepare-pool and path-bucketed gate.
* Build `sandbox/runtime/thin_client.sh` (new) — sandbox-side bash script
  that the host's `process.exec` invokes. Reads the JSON envelope, picks
  the destination socket from the op, pipes the envelope, prints the
  reply. ~50 lines of `sh`.
* `sandbox/control/daemon/install.py` and `command.py` updated to spawn
  two servers (PID files, supervision, latch-to-fork on N consecutive
  failures), and to emit the thin-client invocation rather than the
  current single-daemon thin client.
* `sandbox/control/daemon/bundle.py` includes the new server modules.
* Backwards compatibility: keep the current `runtime/daemon.py` operational
  behind a feature flag for one release cycle.

### Out of scope

* HTTP / FastAPI / uvicorn. Pure JSON-over-AF_UNIX is the right shape for
  one-known-client / one-known-message-format use cases.
* Cross-sandbox sharing of either server. Each sandbox runs its own pair.
* Replacing `process.exec`. Daytona stays in the adapter; this plan
  preserves that invariant.
* Rewriting OCC commit semantics. The merger stays single-writer.

## Wire format

Newline-delimited JSON over AF_UNIX. Identical framing on both servers.

**Request:**

```json
{"op": "<dotted-op-name>", "args": {...}, "request_id": "<uuid>"}\n
```

**Response:**

```json
{"success": true|false, "result": {...}, "warnings": [...], "timings": {...},
 "error": {"kind": "...", "message": "...", "details": {...}}}\n
```

The op namespace splits routing:

| Op prefix | Destination | Notes |
|---|---|---|
| `overlay.run` | overlay-server | mount + exec + capture; returns capture envelope |
| `api.shell` | thin_client (composite) | step 1: `overlay.run` to overlay-server; step 2: `api.commit` to occ-server with the capture; aggregates timings |
| `api.write_file` | occ-server | direct |
| `api.edit_file` | occ-server | direct |
| `api.read_file` | occ-server | direct |
| `api.commit_capture` | occ-server | new — accepts an OverlayCapture envelope, runs prepare + commit |
| `api.pinned_layers` / `api.layer_metrics` / `api.compact` | occ-server | direct |

The `api.shell` composite is the only op that takes two server hops per
call. Both hops happen inside the sandbox (over AF_UNIX), so the host
still pays exactly one `process.exec` round trip per shell.

## Server design — `overlay-server`

**Process model.** A long-lived parent Python process. On startup it
pre-warms a pool of N child processes (default `N=8`, tunable via
`EPHEMERALOS_OVERLAY_POOL_WORKERS`). Each child has its own GIL and its
own preloaded copy of the overlay capture machinery. The parent
dispatches incoming `overlay.run` envelopes to whichever child is idle.

**Why pre-warmed children, not fork-per-request:** the win we want is
fork's parallelism without paying Python interpreter cold start per
call. Pre-warming captures both — children are kept alive, but each
mount + exec + capture runs on its own GIL.

**Stateless.** No file-system state survives across calls. The parent is
stateless except for the pool roster; each child is reset between
requests (the overlay namespace is unmounted after capture).

**Files:**

| File | Role |
|---|---|
| `sandbox/runtime/overlay_server.py` | parent process: AF_UNIX listener, pool supervisor, dispatch |
| `sandbox/runtime/overlay_worker.py` | child process: receives envelope on stdin (or pipe), runs the existing `RuntimeInvoker.invoke` + capture, emits envelope on stdout |
| `sandbox/runtime/overlay_pool.py` | pool primitives — pre-spawn, restart on crash, idle-tracking, drain on shutdown |

**Failure model.** Child crashes are isolated; the parent restarts the
crashed child and the pending request fails with a structured error
(callers retry idempotently). Parent crash takes the whole server down;
operator/control-plane respawns it.

## Server design — `occ-server`

**Process model.** Single Python process. Asyncio event loop accepts
AF_UNIX connections, dispatches to op handlers. The handlers are the
existing `api_handlers.py` logic, ported across with no behavioral
change.

**Owns shared state.** The cached `LayerStackManager`, `OccService`,
`LayerStackGitignoreOracle`, the path-bucketed asyncio.Lock buckets,
and the `OccSerialMerger` worker thread all live here. Single-writer is
exactly what OCC commit semantics require, so this server doesn't need a
worker pool.

**Folds in the prepare-pool.** Phase 3.x.1's `EPHEMERALOS_PREPARE_POOL`
becomes occ-server's default — it's the only place CPU-bound prepare
work lives now.

**Files:**

| File | Role |
|---|---|
| `sandbox/runtime/occ_server.py` | AF_UNIX listener, dispatch loop |
| `sandbox/runtime/occ_handlers.py` | extracted from today's `api_handlers.py` (read / write / edit / commit_capture / metrics / compact) |
| `sandbox/runtime/occ_state.py` | the per-`layer_stack_root` service cache (current `_SERVICE_CACHE`) |

## Server design — `thin_client.sh`

A `sh` script. ~50 lines. Reads the JSON envelope from `$1`, peeks at the
`"op"` field with a tiny `python -c` (or `jq` if available), picks the
socket, pipes through `nc -U` (or `socat`, or a tiny `python -c` AF_UNIX
client like the one in today's `command.py`), captures the reply, prints
to stdout.

For the `api.shell` composite case, the script does it inline:

```sh
# pseudo-code
capture_env=$(echo "$envelope" | thin_client_pipe /tmp/eos/ovr.sock)
[ "$(jq .success <<< "$capture_env")" = "false" ] && { echo "$capture_env"; exit 1; }
commit_env=$(make_commit_envelope "$capture_env" | thin_client_pipe /tmp/eos/occ.sock)
merge_envelopes "$capture_env" "$commit_env"
```

The whole thing runs inside one `process.exec`. Two AF_UNIX hops, one
host round-trip.

## Migration

`EPHEMERALOS_RUNTIME_TRANSPORT` gains a third value: `two_server`.

| Value | Behavior |
|---|---|
| `fork` (default) | unchanged — `python -m sandbox.runtime.server` per call |
| `daemon` | unchanged — current single-process daemon (kept one release for rollback) |
| `two_server` | new — overlay-server + occ-server, supervised separately |

Default flips to `two_server` after the verification gate (below).
`daemon` retained for one release; `fork` stays as the safe floor.

The Phase 3.x.5 supervision logic generalizes: track failures per server,
latch transport back to `fork` when either server keeps failing.

## Verification

### Unit tests

* `backend/tests/unit_test/test_sandbox/test_runtime/test_overlay_server.py`
  — pool roster, child crash → restart, drain on shutdown, framing.
* `backend/tests/unit_test/test_sandbox/test_runtime/test_overlay_pool.py`
  — pre-spawn warmup, idle-tracking, request dispatch fairness.
* `backend/tests/unit_test/test_sandbox/test_runtime/test_occ_server.py`
  — handler dispatch, state-cache reuse across calls.
* `backend/tests/unit_test/test_sandbox/test_runtime/test_thin_client.py`
  — op routing (overlay.* vs api.*), shell composite path, error
  propagation.

### Live latency attribution sweep

Run the existing `test_latency_attribution.py` probe in three modes
back-to-back: `fork`, `daemon`, `two_server`. The decisive comparison is
**`two_server` vs `fork` on shell at c=16**: the whole point of this
work is closing the regression there.

### Pass bar (c=16 p99)

| Metric | Target | Notes |
|---|---:|---|
| shell wall p99 | ≤ 1500 ms | matches or beats fork-mode shell |
| read / write / edit wall p99 | ≤ daemon-mode equivalent | retains the Phase 3+4 wins on those verbs |
| `api.shell.commit_s` p99 | ≤ 100 ms | shell commits funnel through one OCC server but should batch effectively |
| `overlay.invoker.queue_wait_s` p99 | ≤ 50 ms | pool fan-out keeps the queue shallow |
| Drift | 0 | conflict semantics preserved end-to-end |
| Pool RSS after 1000 calls | < 500 MB | bounded by pool size and per-child cap |
| Server crash + auto-restart | recovers within 2 s | no host-visible failure beyond a single retry |
| `from daytona` outside `sandbox/providers/daytona/` | 0 | adapter invariant unchanged |

## Risks

* **Two-hop shell path adds a second AF_UNIX round trip per shell call.**
  AF_UNIX is sub-millisecond locally, so the overhead is negligible (~1
  ms p99) — but it's worth measuring during verification rather than
  assuming.
* **Pool size tuning.** Too few children and shells queue; too many and
  RSS balloons. Default `N=8` is a guess; the live sweep should sweep
  pool sizes once and pick a knee-point.
* **Bundle size growth.** Two server modules + pool primitives + thin
  client script add ~10-15 KB to the bundle. Pre-warming N children
  needs the bundle to be small enough that startup is fast. The current
  bundle is 600 KB; this stays well under the chunked-upload budget.
* **State-ownership drift.** If anyone above the adapter starts caching
  layer-stack state, two-server semantics break. Mitigation: keep the
  state cache exclusively inside `occ-server`; lint rule asserts no
  external module imports `_SERVICE_CACHE`.
* **Restart semantics.** A mid-call OCC server crash drops in-flight
  commits. Today's daemon has the same issue. Mitigation: restart-window
  retry (~500 ms) in the thin client before failing the host call.
* **Operator complexity.** Two PID files, two sockets, two log files.
  Mitigation: a single `eos-sandbox-runtime` systemd-unit-equivalent
  that supervises both as a unit. The control plane already does this
  for one daemon today.

## Effort estimate

| Component | Days |
|---|---:|
| overlay-server: AF_UNIX framing + dispatch | 1 |
| overlay-pool: pre-warm, supervise, dispatch | 2 |
| overlay-worker child protocol + drain | 1 |
| occ-server: extract handlers, port to its own listener | 2 |
| occ-server: prepare-pool integration (lift from today's daemon) | 1 |
| thin_client.sh: routing + composite shell path | 1 |
| install/supervise: two PIDs, two sockets, latch-to-fork | 2 |
| bundle.py changes (include new modules) | 0.5 |
| Unit tests | 2 |
| Live verification sweep + pool-size sweep | 1.5 |
| Documentation + plan-update + report | 1 |
| **Total** | **15 working days** |

That's larger than any of Phases 1-4 individually. Most of the framing
code can be lifted from existing `runtime/daemon.py` and `prepare_pool.py`,
so the actual *new* code is closer to 2/3 of that estimate. The remaining
1/3 is the migration / verification / docs surface.

## What this would unlock

* Daemon mode genuinely beats fork on every verb at every concurrency
  level — closes the only open regression from the latency plan.
* Clean state-ownership boundary makes future work (multi-tenant
  sandboxes, replicated overlay-server, per-layer-stack OCC sharding)
  tractable.
* Operations get two named services with obvious failure modes
  (`overlay-server up?` `occ-server up?`) instead of one opaque daemon
  whose internals are mixed.

## What this would *not* unlock

* The `process.exec` floor stays. Verb-level batching
  (`write_batch` / `edit_batch` / `read_batch`) remains the next
  high-leverage win after this work, and is unaffected by the
  two-server split.
* The OCC merger's single-writer remains a serial bottleneck for very
  high-concurrency commits. Path-sharded OCC servers (one per bucket)
  would be a follow-on if real workloads ever stress that.

## Open questions

1. **Should the thin client be `sh` or compiled?** A statically-linked
   tiny Go binary would cut its startup further. Probably overkill —
   `sh` + `python -c` for the AF_UNIX hop is what we already use and is
   under 5 ms total.
2. **Do we keep the legacy `daemon` mode at all?** If `two_server`
   demonstrably wins everywhere, remove `daemon` after one release. If
   there's a compat case (low-concurrency real workload where the
   single-daemon was already winning), keep it.
3. **Pool size policy.** Static via env var? Or auto-tune based on
   observed concurrency? Static for now; auto-tune later if anyone asks.

## Next steps

1. Review this plan with the team / next maintainer. The architectural
   shift is bigger than Phases 1-4 combined; explicit alignment matters
   more than precision on the file layout.
2. If approved, land in two checkpoints:
   * **Checkpoint A:** overlay-server + thin_client + occ-server stub
     wired to delegate to the existing daemon. Closes the shell regression
     without touching OCC code paths. ~6-7 days.
   * **Checkpoint B:** occ-server takes ownership of state; `daemon.py`
     is decommissioned. ~8-9 days.
3. Update `api-latency-reduction-plan.md` to point at this plan as the
   "Phase 5" / "Beyond Phase 4" successor.
