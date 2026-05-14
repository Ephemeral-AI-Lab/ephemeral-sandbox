# Phase 4 Pure `sandbox.api.*` Latency Attribution

Date: 2026-05-06.

Companion to `phase-04-pure-sandbox-api-load-report.md`. This run does **not**
use `shell_batch` — every call is an independent agent dispatch (one
`provider.exec` per call). The probe is `test_latency_attribution.py`. Per-call
JSONL is at `.omc/results/live-e2e-phase3-per-call-timings-20260505T175743Z-1591.jsonl`.

## New telemetry keys added

- `runtime.boot_to_dispatch_s` — Python interpreter start to handler dispatch.
- `runtime.dispatch_s` — handler execution inside the runtime process.
- `api.shell.process_gate_wait_s` / `api.edit.process_gate_wait_s` / `api.write.process_gate_wait_s` — wait on the per-process `asyncio.Lock` commit gate.
- `api.shell.flock_wait_s` / `api.edit.flock_wait_s` / `api.write.flock_wait_s` — wait on the cross-process `flock(.commit.lock)`.
- `api.shell.overlay_capture_to_changes_s` — overlay capture → OCC `Change` conversion.
- `gitignore.cache_hits` / `gitignore.cache_misses` — per-snapshot oracle cache stats.
- former materialization / git-init timing — cost of building the old git-backed gitignore oracle for a new manifest version.
- `api.read.layer_stack_read_s` — layer-stack `read_text` time inside the runtime.

## Wall p99 by verb × concurrency (ms)

| Verb | c=1 | c=4 | c=8 | c=16 |
|---|---:|---:|---:|---:|
| `read_file` | 508 | 584 | 615 | 931 |
| `write_file` | 588 | 874 | 1199 | 2131 |
| `edit_file` | 606 | 875 | 1232 | 2111 |
| `shell` (`:` baseline) | 772 | 847 | 927 | 1407 |
| `shell` (`echo > file`) | 779 | 843 | 930 | 1438 |

## c=16 attribution (p99 ms)

| Stage | read | write | edit | shell baseline | shell real |
|---|---:|---:|---:|---:|---:|
| Wall | 931 | 2131 | 2111 | 1407 | 1438 |
| `runtime.boot_to_dispatch` (Py interp boot) | 61 | 58 | 66 | 61 | 70 |
| `runtime.dispatch` (handler total) | 14 | 1144 | 1190 | 480 | 463 |
| **Transport gap** = wall − dispatch − boot | **856** | **929** | **855** | **866** | **905** |
| `api.{verb}.flock_wait` | n/a | 1054 | 1097 | 0 | 0 |
| `api.{verb}.process_gate_wait` | n/a | 0.5 | 0.4 | 0 | 0 |
| `occ.prepare.total` | n/a | 98 | 92 | n/a | n/a |
| `occ.commit.total` | n/a | 8 | 6 | n/a | n/a |
| `gitignore.materialize_snapshot` | n/a | 12 | 33 | n/a | n/a |
| `gitignore.git_init` | n/a | 43 | 31 | n/a | n/a |
| `overlay.run_command` (bash + user cmd) | n/a | n/a | n/a | 389 | 398 |
| `api.shell.overlay_capture_to_changes` | n/a | n/a | n/a | 0.007 | 0.005 |

## Findings

### 1. Transport is ~860–930 ms p99 at c=16, regardless of verb
The gap between `wall_ms` and `runtime.boot_to_dispatch + runtime.dispatch` is the
host→sandbox round trip: provider exec, network, sandbox `sh -c $launcher`,
finding/exec'ing python, plus stdout flush back. It scales mildly with
concurrency (c=1 ≈ 460 ms → c=16 ≈ 900 ms), suggesting provider/network queue
saturation rather than per-call cost growth.

### 2. Python interpreter cold start: ~40–70 ms per call, paid every time
`runtime.boot_to_dispatch` is purely the Python interpreter starting up and
importing `sandbox.runtime.daemon.rpc.dispatcher`. At c=16 × 50 ms = 800 ms of sandbox CPU
spent just on Python imports — and it doesn't decrease with concurrency.

### 3. **The single-process flock is the dominant write/edit bottleneck**
- `write_file` flock_wait p99 scales linearly: c=1 → 1.4 ms, c=4 → 224 ms, c=8 → 502 ms, c=16 → **1054 ms**.
- `edit_file` mirrors it: c=1 → 1.3 ms, c=4 → 220 ms, c=8 → 522 ms, c=16 → **1097 ms**.

**Why:** every parallel `write_file`/`edit_file` runtime call holds
`flock(.commit.lock)` while running OCC apply (~70–100 ms hot zone, dominated
by `occ.prepare`). With 16 waiters at ~70 ms hold time, the 16th waiter
queues ~15 × 70 ms = ~1050 ms — matches what we measured.

The `_process_commit_gate` (asyncio.Lock per-process) is essentially free
(<0.5 ms) — concurrent agents arrive in *separate* runtime processes, so the
contention is the cross-process flock, not the in-process gate.

### 4. **`shell` does NOT contend on the flock**
`api.shell.flock_wait` is 0 even at c=16. Reason: shell runs the whole overlay
mount + bash + capture phase **outside** the flock, then takes flock only
briefly for the `_apply_overlay_capture` write. Hold time is ~5 ms, and the
host can't fan out 16 shells fast enough to ever queue them — shell is gated
on transport + bash startup, not on the commit lock.

### 5. **`shell_real` ≈ `shell_baseline` — bash startup dominates `overlay.run_command_s`**
`overlay.run_command_s` p99 at c=16: 389 ms (`:` no-op) vs 398 ms (`echo > file`).
The user command costs <10 ms; the rest is `bash -lc` + namespace overhead.
**Floor cost of any shell is ~390 ms in-runtime, ~1400 ms wall.**

### 6. `read_file` only doubles wall from c=1→c=16
Read does no commit, so no flock and no OCC. The 423 ms growth (508 → 931 ms)
comes entirely from transport saturation. Confirms transport is the variable
cost, not server-side work.

### 7. The former git-backed oracle was cold every call
Each new sandbox runtime process starts with an empty `_oracles` cache. Every
write/edit pays one `materialize_snapshot` + `git_init` (~40–75 ms total).
This is bundled inside `occ.prepare.total_s` (62–98 ms p99), so OCC prepare
p99 is mostly gitignore-bring-up, not OCC routing logic.

### 8. OCC commit core remains fine
`occ.commit.total_s` is 3–8 ms p99 at every concurrency level. Layer-stack
publish < 2 ms. **The transactional core is not the bottleneck.**

## Bottleneck ranking (c=16 p99)

| Bottleneck | Cost | Affected verbs | Mitigation surface |
|---|---:|---|---|
| Cross-process flock serialization | ~1050 ms | write, edit | resident dispatcher / per-path locking / batch the OCC apply |
| Provider exec + network transport | ~900 ms | all | resident runtime worker over a unix socket |
| `bash -lc` namespace startup | ~390 ms | shell | warm bash worker; reuse mount; trim profile |
| OCC prepare (incl. gitignore cold start) | ~95 ms (under flock) | write, edit | cache gitignore across processes; precompute base hash |
| Python interpreter cold start | ~60 ms | all | resident runtime; no per-call `python -m` |
| OCC commit | ~6 ms | write, edit, shell | already fine |
| Layer stack publish | ~1 ms | write, edit, shell | already fine |

## What this means for the next step

The two single largest wins are both about **eliminating per-call process
launch**:

1. Replace per-call `sh -c $launcher python -m sandbox.runtime.daemon.rpc.dispatcher` with a
   resident runtime process (one Python interpreter, kept warm). Removes
   `runtime.boot_to_dispatch` (~50 ms × N), and lets the in-process
   `_process_commit_gate` actually serialize commits in <1 ms instead of 1 s
   via flock.

2. Once resident, every commit holds the in-process `asyncio.Lock` for ~70 ms
   instead of holding flock across processes. At c=16, the 16th waiter
   queues at ~70 ms × 15 = 1050 ms total *but the lock contention vanishes
   if OCC prepare happens outside the gate* — gitignore oracle warm-cache and
   base-hash computation can run before lock acquire, leaving ~5 ms of work
   inside the gate.

Together these two changes would compress write/edit p99 at c=16 from ~2.1 s
toward roughly the c=1 floor of ~600 ms, dominated by transport. After that,
transport is the next target — that's a provider/network problem, not a
sandbox-runtime one.

---

## Update — after Phases 1, 2, 3, 4 (2026-05-06)

The four phases prescribed by `.omc/plans/per-call-snapshot-layer-stack-migration/api-latency-reduction-plan.md`
are all landed (see commits `42bdfde7`, `ad00c87b`, `083bc336`, `fbae5dc2`).
This section records the post-implementation measurements taken with
the same probe (`test_latency_attribution.py`) against the same
sandbox image (`registry:6000/daytona/sweevo-psf-requests-3738:v1`),
sweeping c ∈ {1, 4, 8, 16}. The run uses the resident daemon and the
pathspec gitignore oracle.

### Wall p99 across phases (c=16, ms)

| Verb | Baseline | After P1 | After P1+P2 | After P1+P2+P3 | After P1+P2+P3+P4 | Δ vs baseline |
|---|---:|---:|---:|---:|---:|---:|
| `read_file` | 931 | 931 | 931 | 673 | **722** | **−22 %** |
| `write_file` | 2131 | 1100 | 1032 | 1000 | **717** | **−66 %** |
| `edit_file` | 2111 | 1098 | 1004 | 1255 | **755** | **−65 %** |
| `shell` (`echo > file`) | 1438 | 694 | 694 | 5626 | 3267 | +127 % ⚠ |

### Where the time goes now (c=16 daemon p99)

| Stage | read | write | edit | shell |
|---|---:|---:|---:|---:|
| `runtime.boot_to_dispatch_s` | 0.4 | 0.7 | 0.5 | 9.9 |
| `prepare_s` (lock-free) | – | 65 | 73 | 58 |
| `process_gate_wait_s` (path-bucket, Phase 4) | – | 6.7 | 5.8 | 40 |
| `flock_wait_s` (no-op in daemon) | – | 0.017 | 0.014 | 0.015 |
| `commit_s` | – | 48 | 12 | **699** |
| former materialization / git-init timing | 0 | 0 | 0 | 0 |
| `runtime.dispatch_s` | 3.4 | 92 | 79 | 2663 |
| `process.exec` transport floor (host↔sandbox, structural) | ≈700 | ≈700 | ≈700 | ≈700 |
| **wall p99** | **722** | **717** | **755** | **3267** |

### Sub-cost wins vs baseline (c=16 p99)

| Sub-cost | Baseline | Final | Δ |
|---|---:|---:|---:|
| Cross-process flock contention (write/edit) | 1054–1097 ms | 0.014–0.017 ms | **−99.998 %** |
| Gitignore `materialize` + `git_init` | 76 ms | 0 ms | **−100 %** |
| Python interpreter boot per call | 58–70 ms | <1 ms | **−98 %** |
| Commit-gate queueing (write/edit) | up to 1097 ms | ≤6.7 ms | **−99 %** |
| `runtime.dispatch_s` write/edit | 1144–1190 ms | 79–92 ms | **−92 %** |

### Per-phase narrative

* **Phase 1** moved the flock fence to wrap `commit_prepared` only, taking the 1054-ms hot zone for write down to ~78 ms by running `prepare` lock-free.
* **Phase 2a** moved the gitignore oracle's git workspace from `tempfile.TemporaryDirectory` to `<storage_root>/cache/gitignore-<version>/` with atomic install and depth-bounded eviction. **Phase 2b** added a `pathspec`-backed evaluator that reads `.gitignore` files directly from the snapshot — `materialize_snapshot` and `git_init` collapse to 0 ms.
* **Phase 3** introduced a resident asyncio AF_UNIX daemon. The per-call command becomes a thin `python -c "socket.connect(...)"` client, so `boot_to_dispatch_s` falls from ~60 ms to <1 ms; the in-memory oracle and `Service` cache durably across calls. Phase 3 alone exposed a new bottleneck — single-process commit serialization on an asyncio.Lock — which Phase 4 then fixed.
* **Phase 4** replaced the single asyncio.Lock per `layer_stack_root` with 16 path-hashed buckets and made `write_file`/`edit_file` opt out of `CommitOptions.atomic` (which the codex parallel session flipped to `True` between Phase 3 and Phase 4 and which would otherwise defeat the merger's batching). `process_gate_wait_s` collapses by 96–97 % and the merger's batch window finally coalesces disjoint commits.

### Outstanding regression

Shell (`echo > file`) is the only verb above baseline. The cause is mechanical: `CommitOptions.atomic` defaults to `True`, and `_disjoint_batches` in the serial merger never coalesces atomic items, so 16 concurrent shells go through `revalidate_and_publish` as 16 single-item batches at ~41 ms each. Phase 4 made this opt-out for write/edit (single-path; atomicity is degenerate) but deliberately left shell on the default pending a semantic audit of multi-path overlay captures. Closing the regression is one of:

1. Audit shell overlay capture; if every realistic case is single-path, flip the shell handler to `atomic=False` (cleanest).
2. Make `_disjoint_batches` group disjoint atomic items and propagate per-item failure semantics in the merger (more invasive but preserves multi-path atomicity for real shells).

### Bottleneck re-ranking (c=16 p99, post-Phase-4 daemon)

| Bottleneck | Cost | Affected verbs | Status |
|---|---:|---|---|
| `process.exec` host↔sandbox transport | ~700 ms | all | **structural** under the Daytona-stays-in-the-adapter invariant |
| Shell `commit_s` under atomic=True | ~700 ms | shell only | **open** — see options above |
| `bash -lc` namespace startup | ~360 ms | shell | unchanged from baseline |
| `prepare_s` (content hash + pathspec eval) | 58–73 ms | write, edit, shell | bounded in daemon |
| `commit_s` (write/edit) | 12–48 ms | write, edit | acceptable |
| Path-bucket gate wait | ≤7 ms (write/edit) | write, edit | acceptable |
| flock | ~0 ms | write, edit, shell | eliminated |
| Python interpreter cold start | <1 ms | all | eliminated |
| Gitignore cold start | 0 ms | write, edit | eliminated |

### Runtime contract

The current runtime contract is daemon-only. The latency probe no longer
depends on runtime transport feature flags.

### Where the next leverage lies

With Phase 4 in place, every per-call cost outside the `process.exec`
floor is either negligible (<10 ms) or known-bounded. The only verb
that doesn't follow this rule is shell, and that's a one-line policy
fix away. Reducing the transport floor itself would require either
verb-level batching (`write_batch` / `edit_batch` mirroring the
existing `shell_batch`), agent-side batching, or pushing the agent
inside the sandbox — i.e., the items already flagged as
"Beyond Phase 4" in the plan.
