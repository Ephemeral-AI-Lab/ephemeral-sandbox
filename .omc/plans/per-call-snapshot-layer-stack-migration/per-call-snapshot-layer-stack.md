# Per-Call Workspace Snapshot via Append-Only Layer Stack

## Status

- **E1 — root-caused 2026-05-04: use the depth-100 design. There is no observed kernel cap; util-linux 2.41 `mount(8)` is the failing component.** Side-by-side test on the same sandbox `53a4a9b8-316f-40e8-8849-eb7b60fea3d7` (kernel `6.10.14-linuxkit`):

  | depth | opts.len | `mount(2)` syscall (C) | `mount(8)` binary |
  |---|---|---|---|
  | 5  | 218 | rc=0 | rc=0 |
  | 10 | 353 | rc=0 | rc=32 |
  | 16 | 515 | rc=0 | rc=32 |
  | 30 | 893 | rc=0 | rc=32 |
  | 199 (short relative) | 725 | rc=0 | rc=32 (binary fails ≥ d=20) |

  Direct `mount("overlay", target, "overlay", 0, opts)` from a tiny C probe succeeds at every depth tested, both rootless (`unshare -Urm`) and as root (`sudo`). Confirmed nested overlay-on-overlay also mounts cleanly. Likely cause: util-linux 2.41 routes overlay through the new `fsopen()`/`fsconfig()`/`fsmount()` API which has different `lowerdir` parsing than the legacy `mount(2)` `data` argument path. The earlier "depth ≥ 16 fails" reading was a util-linux artifact, not a kernel constraint.

  **Implication:** the depth-≤14 hybrid fallback is retired. The active assumption is a **depth-100 hard cap** with a syscall-based mount path. Runtime mount code must not use `subprocess.run(["mount", ...])`; it should call direct `mount(2)` through Python `ctypes` or a tiny statically-linked `eos-mount` helper. `stack_overlay/mounts.py` and `stack_overlay/experiments.py` are the immediate experimental callers.

- **E2 full syscall rerun:** depth matrix `{1, 5, 10, 30, 50, 80, 100, 200}`, 1000 iterations/depth, direct Python `ctypes` `mount(2)` inside `unshare -Urm`: 0 failures at every depth. Depth 100 p50 0.15 ms, p95 0.34 ms, p99 0.58 ms; depth 200 p99 0.60 ms. This passes the p99 < 5 ms depth-100 target with wide margin.
- Latest E1–E14 rerun records:
  - live Daytona E1–E3: `.omc/results/stack-overlay-live-20260504-065333.jsonl`
  - local `stack_overlay` doc-count E4–E14: `.omc/results/stack-overlay-suite-20260504-065333.jsonl`
- E3 warm-read result passed the depth-100 warm-read target, but cold-cache data remains partial because `/proc/sys/vm/drop_caches` is permission-denied inside the container.
- Commit-side design (per-path CAS + gitignore-aware last-writer-wins + staleness telemetry + lease budget) is orthogonal to the mount utility issue.

## Latest Rerun — 2026-05-04

All feasible experiments were rerun under the depth-100/syscall assumption.
Performance stats were logged and appended as JSONL immediately after each
experiment completed.

| Experiment | Status | Performance / result |
|---|---|---|
| E1 | pass | direct `mount(2)` worked at depths 1, 5, 10, 30, 50, 80, 100, 200; depth-100 single mount 0.51 ms; `mount(8)` negative control still failed at depth 100 with rc=32 |
| E2 | pass | 1000 mounts/depth; depth 100 p50 0.15 ms, p95 0.34 ms, p99 0.58 ms; depth 200 p99 0.60 ms; 0 failures |
| E3 | partial/pass warm | 10k-file warm reads: depth 1 128.31 ms, depth 100 129.64 ms, ratio 1.01x; cold cache unavailable (`drop_caches` permission denied) |
| E4 | pass | doc-count load: 10 runs, 4800 shell ops, 9600 API edits, 0 violations, 53.99 s total |
| E5 | pass | 15000 commits in 27.33 s; 548.83 commits/s; max depth 79, final depth 41; 0 backpressure; commit p95 0.81 ms, p99 51.87 ms |
| E6 | pass | 100 GC contention runs, 0 errors, 3.02 s total |
| E7 | partial/pass synthetic | synthetic storage: 1128 files, peak 2.08 MB, final depth 41, under 256 MB; real workload replay still needed |
| E8 | blocked | blocked until `stack_overlay` is wired into production shell runtime for old-vs-new E2E benchmark |
| E9 | pass | unreferenced crash dirs `B9999`/`L9999` removed, committed layer still readable, 1.95 ms |
| E10 | partial/pass gated | 10000 OCC-gated iterations, 0 violations, 184.80 s; OCC-skipped merge exceptions not implemented in prototype |
| E11 | blocked | blocked until staleness telemetry (`manifest_lag`, `shell_age_seconds`) is wired into the commit result |
| E12 | blocked | blocked until lease age/pinned-byte/old-manifest/global budget enforcement is implemented |
| E13 | blocked | blocked until gitignore-aware per-path policy classification (§4d) is implemented |

## Goals

1. Every shell tool call sees a **frozen** view of the workspace at call entry; mutations from concurrent OCC commits do not bleed into an in-flight call.
2. Mutations captured from a shell call are funneled through the changeset pipeline. Conflict-sensitive text writes/deletes go through OCC-gated merge, which decides per change to **append** (commit) or **reject** (conflict); gitignored paths use explicit OCC-skipped merge.
3. Designed for **multi-agent high concurrency** — N concurrent shell calls and M concurrent API edits, on a single workspace, with no per-call snapshot copying.
4. Constraints: must run inside a Daytona container. No host FS control, no btrfs/reflink, no FUSE, no provider snapshot, no privileged host mounts. Only `unshare -Urm` + kernel overlayfs.

## Non-goals

- Cross-session durability of layers (squashed to a single tree at session end).
- Cross-host replication / distributed consistency.
- Replacing OCC's algorithm. OCC's gate logic is reused as-is; only the read base and write target change.

---

## Design summary

Replace today's `bind live_root → lowerdir` with an **append-only stack of overlay directories**. No directory in the stack is ever mutated in place. Each shell call mounts a fresh overlay whose lowerdir is the colon-joined manifest of all currently-committed layers; its upperdir is per-call tmpfs. Accepted changesets create a new layer at the top of the stack — never a write into existing state. A background squash worker collapses the bottom of the stack to keep depth bounded.

```
session start                     after K commits
─────────────                     ──────────────────────────────
                                  layer_K       (newest, immutable once created)
                                  layer_K-1     (immutable)
                                  ...           (immutable)
                                  layer_1       (immutable)
layer_0 (workspace baseline) ──── layer_0       (immutable; squash target)
```

Per-call mount:
```
mount(
  "overlay",
  "/workspace_view",
  "overlay",
  0,
  "lowerdir=layer_K:layer_K-1:…:layer_0,upperdir=/per_call_tmpfs/upper,workdir=/per_call_tmpfs/work"
)
```

Drift is gone because the kernel pins the lowerdir set at `mount(2)` time. Even if OCC appends layer_K+1 mid-call, the call's view stays at K.

---

## Components

### 1. Layer storage

- Layers live under `/dev/shm/eos-layers/<session_id>/L<NNNNN>/` (tmpfs for IO speed; falls back to a scratch dir if tmpfs is full).
- `layer_0` is initialized once per session by **bind-mount, not copy**, of the immutable workspace baseline. The baseline itself never receives writes.
- Each subsequent `layer_N` is a directory containing only the **diff** of one accepted changeset or coalesced batch.

### 2. Layer manager (host-side, in-process)

Owns:
- the **manifest** (ordered list of layer dirs from newest → oldest),
- the **refcount table** (mounts → layers in use),
- the **squash worker** (background asyncio task),
- the depth policy: `MAX_DEPTH=100`, initial `SQUASH_TRIGGER=80`, initial `SQUASH_TARGET=40`, and emergency synchronous squash/backpressure before the hard cap.

API:
```python
class LayerManager:
    def snapshot(self) -> Manifest: ...                # O(1), returns frozen list
    def commit(self, layer_dir: Path) -> Manifest: ... # appends, returns new manifest
    def acquire(self, manifest: Manifest) -> Lease: ... # bumps refcounts
    def release(self, lease: Lease) -> None: ...        # drops refcounts; GCs squashed layers
```

**Atomicity:** `commit()` writes the new manifest to a tmp file, `os.replace` over `manifest.json`. Readers re-read on each `snapshot()` and capture the list at that moment. The new layer dir must be fully populated *before* the manifest references it.

### 3. Per-call shell runtime (modifies `overlay/runtime/mounts.py`)

Today's flow:
1. `unshare -Urm`
2. `mount tmpfs` for upper/work
3. `mount --bind live_root → _NS_LOWER`              ← removed
4. `mount overlay -o lowerdir=_NS_LOWER,...`
5. `mount --bind _NS_MERGED → live_root`

New flow:
1. Host: call `LayerManager.snapshot()` → `Manifest`. Pass into the lease.
2. `unshare -Urm`
3. `mount tmpfs` for upper/work
4. direct `mount(2)` overlay syscall with `lowerdir=<colon-joined manifest paths>,...`     ← new lowerdir
5. `mount --bind _NS_MERGED → live_root` (unchanged)

The direct syscall is not an optimization detail; it is required for depth-100.
The current Daytona util-linux `mount(8)` rejects deep overlay lowerdir stacks
that the kernel accepts via the legacy `mount(2)` data argument path.

Capture: same as today. The upperdir tmpfs holds the call's mutations; overlay capture serializes them to `diff.ndjson`.

### 4. OCC commit path (modifies host-side merge code)

Today: the shell pipeline takes captured upper changes, converts them into typed changes, runs OCC for gated change types, and writes accepted content via `ContentManager.write` directly into `live_root`.

New: every shell call follows the same flow — no modes, no caller-declared policy. The changeset pipeline takes captured upper changes, classifies each by path (gitignored vs tracked), applies the corresponding policy (last-writer-wins vs OCC per-path CAS), writes accepted content into a **fresh layer directory** `layer_N+1/`, then atomically `LayerManager.commit(layer_N+1)`.

Universal commit path:

```
1. snapshot()                             → frozen manifest M
2. acquire(M)                             → lease bumps refcounts
3. mount overlay (lowerdir=M, upperdir=tmpfs)
4. run command
5. capture upperdir → diff
6. if diff empty: release(lease); return            ← free fast path
7. for each change in diff:
       if path is gitignored:    policy = last-writer-wins
       else:                     policy = OCC per-path CAS
8. write accepted bytes → L(N+1).staging
9. rename + CAS-publish manifest
10. release(lease)
```

This replaces the earlier 4-mode design (`read_only` / `gated` / `strict_stale` / `exclusive`). The empty-upperdir fast path covers the perf case `read_only` was for. Gitignored last-writer-wins covers the build-artifact case `exclusive` was for. Staleness telemetry is always emitted as a warning but never used as a runtime rejection signal — agents that care about hidden read dependencies handle that app-side.

#### 4a. OCC gate: per-path CAS

For each captured change in shell A's upperdir:

```
base_hash    = hash of path content in shell A's snapshot manifest (version M)
current_hash = hash of path content in active manifest (version M+k)
accept iff base_hash == current_hash
```

Catches write-write conflicts: if B mutated `foo.py` while A was running, A's commit to `foo.py` is rejected.

#### 4b. Staleness telemetry (informational only)

Per-path CAS is the correctness guard. It catches write-write conflicts on the same path, including long shell calls that started from an old snapshot. A long shell with a clean per-path CAS is allowed to commit non-conflicting writes.

Per-path CAS is **not** sufficient for hidden read dependencies in derived outputs. Example:

```
A reads config.yaml v1
B updates config.yaml to v2
A writes generated/output.json (derived from v1)
Per-path CAS sees output.json had no concurrent change → accepts
But output.json's content is semantically stale.
```

Read-set tracking would solve this precisely, but is impractical in Daytona (no fanotify, no privileged eBPF, LD_PRELOAD unreliable for static binaries). The runtime cannot see hidden reads, so it does not pretend to gate them. Instead, the runtime always records staleness telemetry and surfaces it as a warning; agents that care (e.g., codegen consumers) handle the dependency app-side via re-verify or re-trigger logic:

```python
manifest_lag = active_manifest_version - shell_snapshot_version
shell_age_seconds = now - shell_start_ts

result.warnings.append_if_high_lag(manifest_lag, shell_age_seconds)
# Telemetry only — never used as a rejection signal.
```

#### 4c. Per-change semantics

- **OCC-gated path accepts** → resolved bytes land in `layer_N+1`.
- **OCC-gated path rejects (CAS mismatch / anchor miss / existence changed)** → that path's change rejected; other paths in the changeset still evaluated.
- **Gitignored path** → last-writer-wins, no CAS, bytes appended to `layer_N+1` unconditionally.

The gate's CAS read of "current content" walks the manifest top-down to merge the live view (or reads from a host-side cached merged view; see §5). `.gitignore` itself is read from the call's snapshot manifest, not the current manifest, to keep gitignore evaluation consistent with the call's frozen view.

#### 4d. Per-path policy classification

Every captured change first resolves to a normalized workspace path. Invalid paths
are rejected, `.git` internals are dropped, and every remaining path asks the
OCC `GitignoreOracle`. There is no caller-declared mode and no change-kind
bypass to OCC-skipped merge: gitignored paths route to OCC-skipped last-writer-wins;
all other paths enter the tracked OCC gate, where unsupported tracked change
kinds fail closed until their tracked merge policy exists.

| Change type | Path classification | Policy |
|---|---|---|
| UTF-8 regular file write (`WriteChange`) | tracked | OCC per-path CAS |
| UTF-8 regular file write | gitignored | last-writer-wins |
| UTF-8 delete (`DeleteChange`) | tracked | OCC per-path CAS |
| UTF-8 delete | gitignored | last-writer-wins |
| API search/replace edit (`EditChange`) | tracked | OCC anchor-gated |
| API search/replace edit | gitignored | last-writer-wins |
| Non-UTF8 regular file (`BinaryChange`) | tracked | existence + size CAS (best-effort); else last-writer-wins on path-scoped allowlist |
| Non-UTF8 regular file | gitignored | last-writer-wins |
| Symlink (`SymlinkChange`) | tracked | OCC gate; existence CAS target |
| Symlink | gitignored | last-writer-wins |
| Opaque dir (`OpaqueDirChange`) | tracked | fail closed until tracked opaque-dir policy exists |
| Opaque dir | gitignored | last-writer-wins |

Build outputs in `dist/`, `build/`, `target/`, `.next/`, `node_modules/`, `**/*.cache/` etc. are gitignored by convention and therefore flow through last-writer-wins automatically — no mode flag, no workspace mutex required. Tracked codegen outputs (e.g., `*.pb.go`, generated `client.ts`) go through OCC per-path CAS; if a concurrent agent updates the same generated file, the second commit rejects and the agent retries.

### 5. Read paths

Two read consumers:

**(a) The agent API** (`Read`, file listing) — needs the merged view of the workspace.
- Mount a host-process overlay at `/workspace_view_host` with the current manifest as lowerdir, **no upperdir** (read-only). Remount on every manifest change (cheap; ~ms).
- API reads resolve through this view.

**(b) OCC's own CAS check** — needs the same merged view to compute the current content hash.
- Reads the same `/workspace_view_host`. Same code as today, just rooted at the merged view path instead of `live_root`.

### 6. Squash worker

Depth policy for the syscall-based depth-100 design:

| Parameter | Initial value | Purpose |
|---|---:|---|
| `MAX_DEPTH` | 100 | hard manifest cap; no mount should exceed this |
| `SQUASH_TRIGGER` | 80 | enqueue background squash when depth reaches this |
| `SQUASH_TARGET` | 40 | target depth after a successful squash |
| `EMERGENCY_DEPTH` | 95 | stop publishing new write layers and run foreground squash/backpressure |

These are policy caps, not kernel caps. E2/E3/E5 should tune them after the
syscall mount benchmarks. The initial values keep 15 layers of emergency
headroom between background-squash trigger and hard cap, while avoiding the
high squash frequency of the old depth-14 fallback.

Manifest order is newest → oldest. At depth `D`, the worker keeps the newest
`SQUASH_TARGET - 1` layers as live deltas and squashes the older suffix into
one checkpoint layer:

```
before:
  [L099, L098, ..., L061, L060, ..., L000]
   └──── keep newest 39 ────┘ └ squash older suffix ┘

after:
  [L099, L098, ..., L061, B100]
```

Algorithm:

1. **Plan from a frozen manifest.**
   - Read manifest `M`.
   - If `M.depth < SQUASH_TRIGGER`, do nothing.
   - `keep_count = SQUASH_TARGET - 1`.
   - `kept_prefix = M.layers[:keep_count]`.
   - `squash_suffix = M.layers[keep_count:]`.

2. **Build an unpublished checkpoint.**
   - Create `B<next_version>.staging/` on the same filesystem as the layer dirs.
   - Walk `squash_suffix` oldest → newest, applying overlay semantics:
     - regular file write: copy bytes into checkpoint via tmp-file + `os.replace`;
     - whiteout: remove the target path from the checkpoint;
     - directory metadata is best-effort unless later E10 adds metadata CAS.
   - Rename `B<next_version>.staging/` to `B<next_version>/` only after the tree is complete.

3. **Publish with compare-and-swap.**
   - Reload the current manifest `C`.
   - If `C` no longer ends with the same `squash_suffix`, discard the checkpoint and retry later.
   - Otherwise publish:
     ```
     new_layers = C.layers[:-len(squash_suffix)] + (checkpoint_layer,)
     os.replace(manifest.tmp, manifest.json)
     ```
   - This preserves any newer commits that arrived while the checkpoint was being built.

4. **Retire and GC old layers.**
   - Mark `squash_suffix` retired.
   - Do not delete a retired layer while any lease refcount is nonzero.
   - GC deletes only layers that are both absent from the active manifest and unpinned by leases.

5. **Backpressure before hard cap.**
   - At `EMERGENCY_DEPTH`, write-producing commits queue behind a foreground squash.
   - Calls with empty or all-gitignored upperdirs that would not publish a new layer can still acquire leases because they don't grow the stack.
   - If foreground squash cannot make progress because storage is exhausted or manifest CAS keeps losing, return backpressure instead of publishing past `MAX_DEPTH`.

6. **Squash is lease-blind by design.**
   - Layer selection is purely positional. The planner takes the manifest tail as
     `squash_suffix = M.layers[SQUASH_TARGET - 1:]`; lease state is not an input.
   - Layers collapsed per pass = `depth_at_trigger - (SQUASH_TARGET - 1)`. With the
     defaults: trigger 80 → 41 layers fold into 1 checkpoint (depth 80 → 40);
     emergency 95 → 56 layers; hard cap 100 → 61 layers.
   - Squash never **skips** leased layers and does not defer them to a later pass.
     The checkpoint build only **reads** the suffix dirs (no in-place mutation),
     so a leased mount keeps reading from its pinned `lowerdir` correctly while
     the checkpoint is built and the manifest swap publishes the shorter view.
   - After the swap, retired layers split into two groups by refcount:
     unpinned dirs are reclaimed on the next GC sweep; pinned dirs remain on
     disk until the lease releases, then are reclaimed on a subsequent sweep.
     This is the only way a lease extends storage lifetime — it never delays
     the depth reduction itself.
   - Implication: a long-pinning lease cannot prevent depth from being bounded.
     Its cost is bytes-on-disk for retired-but-pinned layers, which is bounded
     by the lease budget caps in §9, not by squash policy.

Crash invariants:
- A checkpoint/staging dir is never referenced before the manifest swap.
- A manifest swap never references a layer dir that does not already exist.
- Startup fsck deletes unreferenced `*.staging`, unreferenced `B*`, and unreferenced `L*` dirs; a manifest that references a missing layer is a hard integrity error.

Runs as a background asyncio task in the normal path. It should not block shell
calls or ordinary commits until the emergency depth guard fires.

### 7. OCC commit coalescing

Under high churn, multiple accepted changesets arriving within `COALESCE_WINDOW_MS` (default 50ms) are batched into one layer:
- Pending diffs accumulate in a staging dir.
- A flush timer (or commit-count threshold) writes the staging dir as `layer_N+1` and atomically appends to manifest.
- All callers waiting on those commits see their results when the batched layer is published.

This caps layer creation rate at ~20/s under sustained burst, even with thousands of commits/sec.

### 8. Layer GC

- A layer is **freeable** when its refcount is zero AND it is no longer in the manifest.
- Background GC sweeps freeable layers. `rm -rf` on tmpfs is fast.
- Worst-case retention: longest-running shell call's lifetime. Bounded by shell `timeout`.

### 9. Lease budget enforcement

Old shells can pin old layers via their leases. Without bounds, a runaway agent could pin GC arbitrarily and exhaust tmpfs. Enforce caps:

| Bound | Default | Behavior on exceed |
|---|---|---|
| `MAX_LEASE_AGE` | shell timeout (typically 600s) | force-kill the shell process; release lease |
| `MAX_PINNED_LAYER_BYTES_PER_SESSION` | 512 MB | new shell calls in that session that would publish a layer block (backpressure) until pin drops |
| `MAX_PINNED_OLD_MANIFESTS` | 16 | oldest pinned manifest's owning shell killed |
| `MAX_TOTAL_PINNED_BYTES_GLOBAL` | 4 GB | global backpressure across all sessions; longest-pinning session evicted |

**Kill semantics.** A killed shell receives `SIGTERM` then `SIGKILL` after grace; its lease releases; its captured upperdir is discarded (no commit). The agent sees `exit_code=-15`, `mutations='killed_lease_overrun'`.

**Per-session vs global.** Per-session caps protect a single agent from monopolizing; global caps protect the host from the aggregate. One runaway agent should not be able to push every other agent into backpressure.

## Concurrency model

**N concurrent shell calls, M concurrent API edits:**

- Each shell call snapshots the manifest at call entry. Its overlay lowerdir is frozen.
- API edits run through OCC, which produces a new layer per coalesced batch.
- The OCC gate's CAS check uses the merged view at *commit decision* time — same semantics as today, just routed through layers.
- Concurrent OCC commits from different agents serialize through OCC's existing per-path lock (`FileChangeApplier._lock`). Layer creation appends; manifest swap is single-writer.

**Drift cases revisited:**
- *Shell A in flight, edit B commits during A's call*: A's lowerdir frozen at snapshot time → A doesn't see B. ✓
- *Shell A and shell B both in flight, A commits before B*: B was snapshotted before A's commit → B sees pre-A view. When B commits, gate sees A's writes in current merged view → B's conflicting writes rejected via CAS. ✓
- *Two shells both write same path concurrently*: first to commit wins via OCC gate; second sees its base hash mismatch → rejected. ✓

---

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Runtime accidentally uses util-linux `mount(8)` | Med | Use direct `mount(2)` or `eos-mount`; keep `mount(8)` only as a negative-control probe |
| Kernel lowerdir depth cap exceeded | Low after E1 root-cause | Keep E1.1 production image verification; fail closed if direct `mount(2)` cannot mount depth 100 |
| `mount` option string > PAGE_SIZE | Low | Short relative layer names (`L00042`), `MAX_DEPTH=100`, squash before length or depth grows unbounded |
| Tmpfs OOM under heavy churn | Med | Disk fallback with stripe layout; per-session tmpfs quota |
| Cold reads slow at depth | Low | Squash bounds depth; kernel dentry cache amortizes hot paths |
| Squash falls behind append rate | Low–Med | Coalescing reduces append rate; background squash starts at depth 80; emergency foreground squash/backpressure starts at depth 95 |
| Nested overlay disallowed in Daytona | Low after E1 root-cause | Keep direct syscall E1 in CI/live validation before production cutover |
| ContentManager rewrite touches many call sites | High | Isolate behind an interface; existing OCC tests cover semantics |
| Layer GC frees a still-referenced layer | High if buggy | Refcount + lease invariants tested in experiment 4 |
| Derived-output staleness on long shells | Med | Staleness telemetry emitted as warning; agents handle hidden read deps app-side via re-verify/re-trigger; E11 validates telemetry accuracy |
| Build outputs lost to OCC false-reject | Low | Gitignored paths bypass OCC entirely (last-writer-wins per §4d); typical build outputs (`dist/`, `build/`, `target/`, `.next/`) qualify automatically |
| Tracked codegen race produces stale committed output | Med | Per-path CAS rejects concurrent updates to the same generated file; agent retries; for hidden-input dependencies the consumer re-triggers codegen |
| Runaway agent pins layers, exhausts tmpfs | Med | Lease budget caps + force-kill (§9); tested in E12 |
| ContentManager `write_text/write_bytes` truncate-in-place | Med | Layer writes and checkpoint writes must use tmp-file + `os.replace`; tested in E9/E10 |

---

## Migration path

1. Build `LayerManager` standalone with unit tests. Squash and refcount logic verified in isolation.
2. Replace shell-out overlay mounts with direct `mount(2)` / `eos-mount`; keep `mount(8)` only as a diagnostic comparison path.
3. Plumb `LayerManager.snapshot()` into the overlay lease; rewrite `mounts.py` to use the manifest. Behind a feature flag, keep the old bind-live_root path working.
4. Reroute accepted changesets to write into a fresh layer dir instead of `live_root`. Keep ContentManager API stable; swap only the resolved-path target.
5. Add the host-side merged-view overlay for API reads + OCC CAS reads.
6. Cut over per session via flag; measure (see experiments below).
7. Remove the old code path once experiments pass at scale.

Effort estimate: ~800–1000 LOC including tests. ~1 week of focused work.

---

## Experiments

Numbered list of what we must verify, in order. Each has a pass/fail bar.

### E1 — Nested overlayfs viable inside Daytona

**Question:** Can we mount overlayfs inside `unshare -Urm` inside a Daytona container, given the host's existing overlay2 root?

**Method:** Minimal repro inside Daytona. Create three dirs on tmpfs, mount overlay, write through, read back, capture upper.

**Pass bar:** mount returns 0; reads/writes correct; no kernel error logs. Repeated with manifest depths of 1, 5, 50, 100.

**Fail mode:** if Daytona's container blocks nested overlay (some seccomp profiles restrict it), the entire design is dead in the water. **Run this first; do nothing else until it passes.**

#### E1 result (dev sandbox `53a4a9b8…`, kernel `6.10.14-linuxkit`)

Root-cause result: the kernel accepts depth 100+; util-linux `mount(8)` is the
failing component.

| depth | opts.len | direct `mount(2)` | util-linux `mount(8)` |
|---|---:|---|---|
| 5 | 218 | pass | pass |
| 10 | 353 | pass | fail rc=32 |
| 16 | 515 | pass | fail rc=32 |
| 30 | 893 | pass | fail rc=32 |
| 199 (short relative) | 725 | pass | fail at d ≥ 20 |

Direct `mount("overlay", target, "overlay", 0, opts)` succeeds both inside
`unshare -Urm` and as root. Nested overlay-on-overlay also succeeds. The likely
cause is util-linux 2.41 routing overlay mounts through `fsopen()` /
`fsconfig()` / `fsmount()` with different `lowerdir` parsing than the legacy
`mount(2)` data argument path.

**Decision:** proceed with the depth-100 design and make direct `mount(2)` the
runtime contract. Production image verification should still record kernel,
util-linux version, and direct syscall depth `{1, 5, 10, 30, 50, 100, 200}`, but
it is no longer a reason to keep the depth-14 hybrid in the plan.

### E2 — Snapshot cost vs depth

**Question:** Does per-call mount latency stay sub-millisecond as depth grows?

**Method:** Microbenchmark: build N layers, measure wall time of direct
`mount(2)` overlay + `umount2`, average over 1000 mounts. Test depths `{1, 5,
10, 30, 50, 80, 100, 200}`. Depth 200 is an overshoot probe only; production
policy caps at 100.

**Pass bar:**
- p99 < 5 ms at depth 100.
- depth-200 probe documents superlinearity if any.

**Fail mode:** if mount latency grows superlinearly within the supported range, squash trigger needs to be tighter. If catastrophic at the trigger threshold, the model isn't viable for high churn under the chosen design.

#### E2 preliminary result (dev sandbox `53a4a9b8…`)

Direct Python `ctypes` `mount(2)` inside `unshare -Urm`, depth 100, 100
iterations:

| depth | iterations | failures | opts.len | p50 | p95 | p99 |
|---:|---:|---:|---:|---:|---:|---:|
| 100 | 100 | 0 | 741 | 0.54 ms | 0.87 ms | 1.25 ms |

This is a preliminary pass for the p99 < 5 ms target. Still run the full
1000-iteration matrix across `{1, 5, 10, 30, 50, 80, 100, 200}`.

### E3 — Cold/warm read latency vs depth

**Question:** What's the read penalty for a deep stack?

**Method:**
- Build a workspace with realistic file count (e.g., 10k tracked + 100k gitignored deps).
- Create N layers each touching a small fraction of files.
- Cold-read benchmark: drop caches, `find /workspace_view -type f | xargs cat > /dev/null`, time it.
- Warm-read: repeat without cache drop.

Depth set: N ∈ `{1, 5, 10, 30, 50, 80, 100}`.

**Pass bar:**
- warm reads within 2× of N=1 baseline at N=100.
- cold reads within 5× of N=1 baseline at N=50.

**Fail mode:** if cold reads grow linearly with N, lower `SQUASH_TRIGGER` or
use a host-side merged-view cache. If warm reads degrade significantly, the
layer-walk overhead is real and the host-side cache becomes mandatory.

### E4 — Correctness under concurrent agents

**Question:** Does the model preserve OCC semantics with N concurrent shells and M concurrent API edits?

**Method:** Stress harness with synthetic agents:
- 8 shell calls/sec sustained for 60 sec, mixed with 16 API edits/sec.
- 50% targeting overlapping paths, 50% disjoint.
- After run: assert (a) no torn reads (every captured upperdir reflects a consistent view of some manifest snapshot), (b) every OCC-accepted write is visible in the final merged view, (c) every OCC-rejected write left no trace.

**Pass bar:** zero correctness violations across 10 runs.

**Fail mode:** if torn reads occur, the manifest swap or refcount logic has a race. Critical bug; redesign.

### E5 — Squash throughput vs append rate

**Question:** Can the squash worker keep up with realistic churn?

**Method:** Sustain 50 accepted changesets/sec for 5 min with `MAX_DEPTH=100`,
`SQUASH_TRIGGER=80`, `SQUASH_TARGET=40`, and `EMERGENCY_DEPTH=95`. Measure:
stack depth over time, squash duration distribution, coalescing ratio, and
backpressure rate.

**Pass bar:**
- stack depth stays in `[40, 90]` during sustained load after warmup.
- zero emergency-depth events.
- zero backpressure events in the normal 50/sec workload.
- coalescing publishes no more than ~20 layers/sec under burst load.

**Fail mode:** if depth grows monotonically toward 100, squash is IO-bound below
the append rate. Tune coalescing tighter, lower trigger/target, or accept a
hard append-rate cap before production.

### E6 — Layer GC under contention

**Question:** Are squashed layers correctly retained while in-flight calls still reference them?

**Method:** Hold a long shell call (e.g., 30 sec sleep with file open inside the overlay) while squashing the layers it depends on. Verify the call still reads correctly throughout, layers are freed only after the call exits.

**Pass bar:** zero "file not found" or "stale handle" errors in 100 runs.

**Fail mode:** refcount bug. Critical; redesign.

### E7 — Tmpfs sizing under realistic agent workloads

**Question:** How much tmpfs do we need per session?

**Method:** Replay representative traces (codeact_tool, dep-install, parallel test) and measure total layer storage at peak, before and after squash.

**Pass bar:** peak storage < 256 MB for typical session; clear scaling formula for large sessions.

**Fail mode:** if peak exceeds session tmpfs budget, fall back to scratch dir for cold layers, keep hot top-of-stack on tmpfs.

### E8 — End-to-end perf vs today's design

**Question:** Net effect on shell-call wall time across a real agent workload.

**Method:** Run the existing benchmark suite (the 100-load workload referenced in the gitignored-deps blocker memory) on both the old design and the new design, both correctness-fixed.

**Pass bar:** new design ≤ 1.2× old design's median wall time, ≤ 1.5× p99. Drift incidents in old design's logs reduced to zero in new design.

**Fail mode:** any regression > 1.5× → profile and tune before cutover.

### E9 — Failure recovery

**Question:** What happens when the squash worker crashes? When the runtime is killed mid-commit?

**Method:** Inject crashes at:
- mid-squash (partial merged_layer dir written, manifest not yet swapped)
- mid-commit (layer dir partially populated, manifest not yet swapped)
- After fault, restart and verify session integrity.

**Pass bar:** no manifest references a non-existent layer; no layer is leaked permanently; in-flight call's lease is cleaned.

**Fail mode:** any orphaned-layer or dangling-manifest case → add fsck-on-startup or write-then-rename invariants.

### E10 — OCC-gated and OCC-skipped merge correctness

**Question:** Does the commit pipeline accept compatible commits and reject conflicting ones across the OCC-gated vocabulary, while OCC-skipped merge exceptions are explicit and bounded?

**Method:** Stress harness with two shells targeting overlapping paths.
- Variants: shell A writes path X; shell B writes path X first → A rejected. Shell A edits X with anchor; shell B writes X first with new content → A rejected (anchor moves). Shell A deletes X; shell B already deleted X → A's delete is no-op. Shell A creates X; shell B already created X → A rejected on existence change.
- Mix in non-conflicting paths (Y, Z) — must always accept regardless of B's activity on X.
- For non-UTF8 files, symlinks, and opaque dirs, verify either equivalent CAS/existence checks or documented last-writer-wins behavior limited to allowed paths (for example gitignored caches).
- Run 10k iterations across all OCC-gated combinations and a focused OCC-skipped exception matrix.

**Pass bar:** zero false-accept for OCC-gated changes (rejected change should never land), zero false-reject for compatible gated changes. OCC-skipped merge exceptions must either be guarded or produce only documented, path-scoped last-writer-wins results. Existing OCC tests cover most semantics; this verifies they hold under the layer-stack base view.

**Fail mode:** false-accept = correctness bug, must redesign. False-reject = perf hit, tune CAS read.

### E11 — Staleness telemetry accuracy

**Question:** Does the runtime correctly accept OCC-clean long-shell writes regardless of manifest age, and does it surface accurate staleness telemetry so agents can decide whether to retry?

**Method:** Synthetic workload encoding the canonical scenario:
1. Shell A reads `config.yaml` v1 at snapshot version M.
2. Concurrent commits advance manifest to M+k while A runs (no commits to A's write paths).
3. Shell A writes `generated/output.json` (derived from config v1).
4. A commits.

Vary k ∈ {1, 2, 4, 5, 6, 10, 20} and shell duration ∈ {5s, 30s, 60s, 120s}.

**Pass bar:** A's write to `generated/output.json` is **always accepted** (per-path CAS is clean). The result includes `manifest_lag = k` and `shell_age_seconds` in the telemetry block. No commit is rejected solely because of age or lag.

**Fail mode:** any age/lag-based rejection at the runtime layer is a regression — agents must own the staleness decision app-side.

### E12 — Lease budget enforcement

**Question:** Do lease caps prevent runaway pinning without breaking legitimate long shells?

**Method:** Inject pathological workloads:
1. Shell that exceeds `MAX_LEASE_AGE`: verify SIGTERM/SIGKILL; lease released; upperdir discarded; agent receives `mutations='killed_lease_overrun'`.
2. Session producing layers totaling > `MAX_PINNED_LAYER_BYTES_PER_SESSION` of pinned diffs: verify backpressure on new shells that would publish a layer; calls with empty or all-gitignored upperdirs that don't grow the stack are unaffected.
3. More than `MAX_PINNED_OLD_MANIFESTS` concurrent old-snapshot shells: verify oldest is killed at `MAX_PINNED_OLD_MANIFESTS+1`.
4. Global pin > `MAX_TOTAL_PINNED_BYTES_GLOBAL` across multiple sessions: verify longest-pinning session is evicted; other sessions continue.

**Pass bar:** caps fire deterministically; no GC starvation; kill semantics consistent.

**Fail mode:** if legitimate workloads trip caps frequently, defaults are wrong; tune from real telemetry.

### E13 — Gitignore-aware policy classification

**Question:** Does §4d's per-path classification correctly route gitignored writes through last-writer-wins and tracked writes through OCC, without leakage between the two?

**Method:** Test matrix:
- Tracked source file (`src/foo.py`) edited by two concurrent shells → second commit OCC-rejected. ✓
- Gitignored cache file (`.pytest_cache/v/cache/lastfailed`) written by two concurrent shells → both commits accepted; last-writer wins; no CAS overhead measured. ✓
- Build output in conventional gitignored dir (`dist/bundle.js`, `target/release/foo`, `.next/cache/x.json`) written by two concurrent agents → both accepted; final state is the later writer's bytes. ✓
- Mixed changeset: shell writes `src/foo.py` (tracked) + `dist/foo.js` (gitignored) concurrent with another shell's `src/foo.py` write → tracked path rejects per CAS, gitignored path commits anyway. ✓
- `.gitignore` itself updated mid-flight: verify each call's classification uses the snapshot-time `.gitignore`, not the live one.

Run 10k iterations across the matrix.

**Pass bar:** zero leakage — no tracked path commits without CAS; no gitignored path is ever rejected on CAS grounds. Mixed changesets correctly partial-commit by per-path policy.

**Fail mode:** any classification leak is a correctness bug. Wrong-direction leaks (tracked → LWW) silently corrupt source; wrong-direction leaks (gitignored → CAS) cause spurious build failures.

---

## Open questions for review

1. **Production image verification (belt-and-braces).** What kernel and util-linux versions does production Daytona run? Direct `mount(2)` depth 100 should still be recorded before production cutover, but the active design no longer branches on a depth-14 fallback.
2. **Layer storage location.** `/dev/shm` tmpfs (RAM, fast) vs persistent scratch dir (survives, slower). Default tmpfs?
3. **Squash thresholds.** `SQUASH_TRIGGER=80`, `SQUASH_TARGET=40`, and `EMERGENCY_DEPTH=95` are initial values. E2/E3/E5 should tune them.
4. **Coalescing window.** 50ms is a guess; tradeoff between commit latency and layer-creation rate. Should be configurable per-session.
5. **API read view caching.** Remount-on-manifest-change is cheap but stat-heavy under high churn; consider an in-process merged-view cache that the host walks instead of remounting. Decide after E3.
6. **ContentManager scope.** This design assumes ContentManager is rewritten to read merged view + write to fresh layer dir. Some callers still expect `live_root` semantics — they need an adapter. Layer/checkpoint writes should use tmp-file + `os.replace`.
7. **Session boundaries.** Layer stack lifetime = agent session. At session end, do we squash everything down to a single layer and write back to `live_root` (slow but durable), or keep the stack across sessions (faster but fragile)?
8. **Staleness telemetry thresholds.** `manifest_lag` and `shell_age_seconds` are surfaced unconditionally; what (if any) advisory thresholds should the runtime emit warnings at? Telemetry never rejects, so this only affects log/UX volume. Defaults TBD from E11 traces.
9. **Lease budget defaults.** `MAX_PINNED_LAYER_BYTES_PER_SESSION=512MB`, `MAX_TOTAL_PINNED_BYTES_GLOBAL=4GB`, `MAX_PINNED_OLD_MANIFESTS=16` — all guesses; E12 should set them from observed peak pinning across representative sessions.
10. **Gitignore evaluation timing.** Use snapshot-time `.gitignore` (cached at call entry, consistent with the call's frozen view) or commit-time `.gitignore` (live, tighter)? The plan currently picks snapshot-time for consistency with the rest of the design. E13 should confirm no edge cases break.
11. **OCC-gated tracked non-text types.** `BinaryChange` and `SymlinkChange` on tracked paths currently use existence/size CAS as a best-effort proxy. Should we promote them to full content-hash CAS (cost: hashing per binary) or accept best-effort? Decide after E10 measures false-accept rate on real binary edits.
12. **Tracked codegen ergonomics.** When `protoc` / `prisma generate` / `openapi` writes a tracked output and another agent has updated the same file, the second commit rejects and the agent must retry. Is the retry contract per-call (agent re-runs codegen) or pipeline-level (runtime auto-replays)? Initial recommendation: per-call, surface the rejection; consider a `retry_on_path_conflict=N` opt-in later.
