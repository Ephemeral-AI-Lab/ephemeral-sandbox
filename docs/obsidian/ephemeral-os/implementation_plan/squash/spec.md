---
title: LayerStack Squash + Live Workspace Remount
tags:
  - ephemeral-os
  - layerstack
  - workspace
  - namespace
  - storage
status: implementation_plan
updated: 2026-07-02
---

# LayerStack Squash + Live Workspace Remount

Revised after the 2026-07-02 adversarial multi-agent review
(`adversarial_multi_agent_review_results.md`) and the same-day simplicity
review (`simplicity_review_results.md`): the substitution ledger moves
in-memory, the restore ladder collapses to an EBUSY park rule, the
persist-failure fallback and real-path mount mode are deleted, commit
durability is one `syncfs`, squash writes zero S-layer sidecars, and the
per-session admission gate subsumes the global lifecycle lock. There is no
auto-squash trigger policy: this spec designs only the manually invoked
squash + live-remount algorithm.

## Goal

Bound LayerStack storage and lowerdir chain length **per invocation** by
squashing published layers into equivalent flattened layers, and migrate
**live** workspace sessions onto the compact chains so old layers reclaim
immediately instead of at session destroy.

Policy (inherits the verdict of the removed live-remount experiment,
`docs/layerstack-command-lease-live-remount_SPEC.md` @ `b6a1de0ac`, remapped
onto the current architecture where the shell runner lives in
`namespace-execution`):

```text
Squash squashes every squashable block; zero options, zero trigger policy.
Squash is singleflight per layerstack root: one guard spans plan → sweep.
Never mutate or delete a layer directory any live lease references.
Storage commit is the correctness boundary; it never depends on live remount.
At commit, re-read the latest manifest and replace only a still-contiguous
planned source run; reclaimed-vs-leased is decided by lease GC at that
instant, never by a plan-time snapshot.
Live remount is best-effort post-commit cleanup; pre-PONR skip/failure keeps
the old lease and never fails a committed squash.
Live remount only after kernel-gated mount proof, all-entrypoint admission,
all-task quiesce, and zero-pin proof.
Retarget lease metadata only as a record of a verified mount state.
Blocked session = healthy: keep its lease, next squash catches it.
Strict-unmount EBUSY after a verified switch = parked: resume on the new
mount holding both leases; the old mount and its lease release at destroy.
Any other failure past the point of no return = faulty: report it, then
destroy it through the ordinary destroy path.
```

Environment facts (normative, load-bearing for recovery):

1. **Holders die with the daemon.** `spawn_ns_holder` sets
   `prctl(PR_SET_PDEATHSIG, SIGKILL)` in a `pre_exec` hook so this is
   kernel-enforced, not deployment luck (today only the pid-ns init carries a
   death signal, bound to the holder — `holder/namespace.rs:189`).
2. **No session survives a daemon restart.** `manager.json` identifies
   leftovers to clean at boot; it is never used to recover sessions.
   Uncommitted upperdir writes share the session's ephemeral fate by design.
3. **No remount state is ever persisted.** Substitution records live in a
   per-root in-memory map and die with the daemon. The post-switch
   `persist_handles()` refresh is best-effort: nothing reads `manager.json`
   beyond boot reap's run-dir locations. There is no quarantine mechanism.

Live remount is gated, not assumed. If any gate below is not proven in the
supported Docker sandbox environment, squash still commits compact layers but
leaves live sessions leased:

1. **Same-upperdir staged mount proof**: OLD and NEW overlays may coexist with
   the same upperdir — NEW always using a **fresh sibling workdir** — long
   enough for the staged `MS_MOVE` switch. The staged mount is built by the
   same production fsconfig builder as creation, so `userxattr`/no-`index`
   parity holds by construction; equivalence is proven by behavioral witness
   reads, never mount-option introspection. The removed experiment's
   visible-options helper lacked `userxattr` and reused the live workdir;
   neither defect may be copied.
2. **All-task quiesce proof**: every task that can observe the holder mount
   namespace is stopped or allowlisted infrastructure. A command pgid is only
   a seed, never the whole proof.
3. **Startup cleanup proof**: leftover handles and run dirs are reaped before
   any boot storage sweep.

CLI surface (manager-owned operation, target sandbox argument):

```text
sandbox-cli manager checkpoint_squash --sandbox-id SANDBOX_ID
```

The manager command forwards one daemon-local runtime request,
`squash_layerstack`, to the selected ready sandbox, delegating to the
existing generic forward path (`router/forward.rs`: endpoint lookup, Ready
check, timeout) — `destroy_sandbox` is a manager-local lifecycle op, not a
forwarding template to copy. The manager spec lives under the existing
`"management"` family (no new family). `squash_layerstack` registers with
`cli: None` on the existing `OperationEntry` field, so it dispatches by name
but never appears in the runtime CLI catalog — no new entry mechanism.

Output contract (one JSON line on stdout; faults on stderr as `{"error":…}`):

```json
{
  "manifest_version": 14,
  "squashed_blocks": [
    { "squashed_layer_id": "S000014-2a",
      "replaced_layer_ids": ["L000011-…", "L000010-…"],
      "replaced_layers": "reclaimed" },
    { "squashed_layer_id": "S000014-2c",
      "replaced_layer_ids": ["L000002-…", "L000001-…", "L000000-…"],
      "replaced_layers": "leased",
      "blocked_reasons": ["pinned:cwd_pinned_workspace"] }
  ]
}
```

`squashed_blocks` = this run's blocks vs lease state; `replaced_layers` is
derived from what the commit-time GC actually deleted (the plan-lease
release returns the removed set) plus the post-sweep lease registry.
`faulty_sessions` (omitted when empty) carries
`{session_id, class_detail, lease_errors}` for every destroyed session —
the result line is the guaranteed surface; faulty outcomes are never
observability-only. `blocked_reasons` strings are free-form `class:detail`
diagnostics; the only contract is a non-empty array when `replaced_layers`
is `leased`. No `layers` dump (the daemon `layerstack` observability view
already serves the stack), no `leases` count (the same view serves
per-layer lease counts, and any count is stale at print time), no `no_op`
flag, no byte totals — byte accounting stays with the observability view.

## Vocabulary and invariants

| Name | Meaning |
| --- | --- |
| boundary | `Base` (never squashed), or the newest layer of some live lease's manifest, computed from the existing `LeaseRegistry::lease_newest_layers()` under the plan lock. A predicate, not a new type. ("pin" is reserved for quiesce inspection findings.) |
| `SquashBlock` | Maximal contiguous run of ≥ 2 active-manifest layers with no boundary inside. |
| plan lease | An ordinary `acquire_snapshot` lease of the plan-time manifest. Its job is to be the zero-new-code GC trigger: releasing it inside the commit — via the existing `release_lease` refcount path, extended to return the removed set — deletes exactly the replaced sources no other lease pins. Sources stay in the active manifest for the whole build (publishes only prepend), so nothing can reclaim them mid-build; the lease is not what pins the build. |
| `flatten` | Pure fold of a block's layer dirs into one changeset, newest-wins per path/subtree. Emits: an explicit entry for **every directory surviving in the block's merged view**; whiteouts/opaques only when they are the winner needed to mask lower layers, classified via `is_kernel_whiteout_meta` + the opaque marker and re-emitted through `write_kernel_whiteout` (both on-disk encodings accepted); regular-file winners as mode-preserving `WriteFile`, **hardlinked** from the immutable source when whole-file. Source walks are fd-relative and no-follow. |
| `SquashedLayer` | The one new layer per block; id prefix `S` (`S000014-2a`). |
| substitution map | In-process per-root map `{S-id → replaced raw run}` recorded at commit, living beside the lease registry (the `shared_registry_for_root` precedent) and dying with the daemon — no sidecar, no schema version, no corruption cases. Raw runs may contain S ids; there is no build-time expansion. A missing entry degrades that substitution to identity. |
| storage commit | Storage-only transaction that builds/promotes `S` layers and atomically replaces `manifest.json`; live remount is not part of this correctness boundary. |
| commit recheck | Inside the commit's exclusive critical section, re-read the **latest** manifest. If the planned source run is still present and contiguous, replace it and commit `version = latest + 1`; otherwise abort, surfaced as the existing `operation_failed` — no dedicated error kind; under normative singleflight the conflict is defensive and unreachable, since publishes and amend only prepend. |
| post-commit remount sweep | Best-effort cleanup after storage commit, inside the same singleflight guard, that tries to move live sessions to compact chains and release old leases. It cannot undo or fail a committed squash. |
| new publish tail | Layers published after squash planning starts but before commit; they stay after the squashed layer and are compacted by a later run. |
| live-session retained space | Old layer bytes kept only because live sessions still use old lease chains; removed by successful remount or session exit/destroy. |
| faulty remount | Any failure at/after the first successful `MS_MOVE` other than strict-unmount EBUSY after a verified switch — including a missing or ambiguous runner report at/past that point: report it (session id, `class:detail`, lease-release errors — no byte totals), then destroy the session through the ordinary destroy path. Namespace death is the unmount proof that lets both leases release. |
| rewritten manifest | Oldest-generation-first contraction, single bounded pass: replace every contiguous run of the lease manifest matching a substitution-map raw run with its S layer, applying map entries in recording order (raw runs may contain earlier S ids, so oldest-first composes across generations without any expansion step). Same logical snapshot, shorter chain; terminates by construction; missing entries ⇒ identity. |
| `acquire_rewritten_lease` | One call under one **shared** writer-lock guard: map contraction, validate rewritten layers alive, acquire the replacement lease — or return `Identity`. Never releases the old lease. |
| old-lease release | The existing `release_lease` under the **exclusive** writer lock, called only after visible mount verification, strict rollback unmount, and task resume (never in the EBUSY park case, where it waits for destroy); refcount GC deletes what nothing references. Not a new lease API. |
| quiesce | Discover every task that can observe the holder mount namespace — union of session cgroup members, a full `/proc` scan for `ns/mnt == holder`, and the infrastructure allowlist (ns-holder, pid-ns init, remount runner: exempt from freeze and from pin inspection — daemon-owned code; a missed infra pin surfaces as the EBUSY park, never corruption). SIGSTOP the rest, poll `/proc/*/stat` to `T` within the freeze budget, verify membership stable. Any discovered task in a different mount namespace blocks (`pinned:mount_namespace_escaped`). The command pgid is only a discovery seed. Child mounts are checked from **one** holder `mountinfo` read per session — `ns/mnt` equality makes every frozen task's mount table the holder's. |
| pin | A frozen task's `cwd`/`root`/open fd/mapped file/child mount inside the workspace mount, an un-allowlisted `anon_inode` fd (io_uring, fanotify, …), a tracer outside the frozen set, or **any** inspection read error. Any pin ⇒ no live switch. |
| staged switch | Kernel-gated mount of the NEW overlay at staging — same upperdir, fresh sibling workdir, production builder — then remask, probe via pre-opened dirfds, MS_MOVE old→rollback, MS_MOVE staging→root, probe, strict-unmount rollback (EBUSY ⇒ park with both leases). Masks are restored **before** the first move, so mask failure is a clean pre-PONR skip. |
| strict unmount | A single `umount2(path, 0)` with no lazy/`MNT_DETACH` fallback. Only strict unmount or namespace death counts as "unmounted". |
| parked old mount | Strict-unmount EBUSY after a verified switch: the OLD overlay stays mounted at the masked rollback point; the session resumes on NEW holding **both** leases (released at destroy), reported `leased(pinned:rollback_unmount_busy)`. The next squash sees Identity — no retry loop, no restore machinery. |
| point of no return | The first `MS_MOVE` **returning success**. Failures before it — including a failed first `MS_MOVE` — leave the session untouched (clean skip). At/after it: verified switch (parked on EBUSY) or faulty. The runner always returns a report of two booleans plus free-form detail (`first_move_succeeded`, `mount_verified` — present on error paths too); a missing or ambiguous report at/past first-move-success is treated as faulty. |

Invariants:

1. **Merged-view equivalence** — for every path, the merged view of the
   rewritten/post-squash manifest equals the pre-squash view, explicitly
   including directory-only shapes (a directory created then emptied survives
   flatten). Squash changes structure, never content; remount is invisible to
   session semantics.
2. **Pin-overlap** — the replacement lease is acquired before the old one is
   released; no instant exists where either chain is unpinned. Clean aborts
   release only the replacement lease.
3. **Never-straddle** — lease manifests are contiguous history prefixes and
   blocks never cross plan-time pins, so every substitution is fully-inside or
   fully-disjoint for every lease, any generation. Every session is always
   rewritable (possibly to identity).
4. **Detach-before-delete** — old lowerdirs are deleted only via lease
   release after the old mount is strictly unmounted or its namespace died.
   Lazy detach never counts as proof.
5. **Upperdir sanctity** — remount never touches the upperdir. The workdir is
   per-mount kernel-transient state: the staged NEW mount always gets a fresh
   nonce-named sibling workdir (`work-remount-<nonce>`); retired workdirs live
   under `run_dir` and are reclaimed by session destroy or boot reap — no
   eager deletion step. The same-upperdir kernel gate proves OLD and NEW coexist
   with production-equivalent options (`userxattr`, no `index`); without that
   proof, live remount is disabled.
6. **Ordering contract**:
   `build → stage → verify staged → switch → verify visible → strict-unmount
   rollback → refresh handle (best-effort) → resume → release old
   lease/refcount GC`.
   Never retarget first; never release the old lease before tasks can run
   again; never release the old lease while the old superblock is alive —
   strict unmount or namespace death is the proof, and EBUSY parks both
   leases until destroy. The handle refresh is the existing
   `persist_handles()`; nothing reads it beyond boot reap, so its failure
   changes nothing.
7. **Session lifecycle is not squash's business** — remount never
   creates/captures/publishes. The single exception: a session past the point
   of no return that is neither verified nor parked is reported as faulty and
   then destroyed through the ordinary session-destroy path.
8. **Commit/cleanup separation** — phases 1–3 may fail the storage commit;
   phase 4 only changes garbage-collection state and live-session retained
   space. A remount skip/failure before the point of no return never rolls
   back, retries, or fails a committed storage commit.

Lock discipline: sessions-map mutex < per-session admission gate < storage
writer lock — the **complete** order: the existing global
`session_lifecycle_lock` (`command/service/core.rs`) is subsumed by the
per-session gate and deleted (nothing it serializes is cross-session).
Never wait on the admission gate while holding the sessions-map mutex; never
hold the storage writer lock across quiesce or the staged switch
(`acquire_rewritten_lease` is shared; only release/GC and the commit are
exclusive).

---

## A. Expected file/folder structure with LoC change

`(new ~N)` = new file with estimated LoC; `(+N)` = lines added to existing
file. Calibrated against existing module sizes (`publish/plan.rs` 241,
`publish/model.rs` 147, service impls 26–64).

```text
crates/sandbox-runtime/layerstack/
├── src/stack/squash.rs                      (new ~250)  plan → build → commit; block model; owns
│                                                        its own ~25-line commit tail (recheck-
│                                                        first); commit GC = plan-lease release
│                                                        (removed set); reclaim classification
├── src/stack/squash/flatten.rs              (new ~180)  layer-dir walk → winning changeset (pure):
│                                                        dir entries, whiteout re-emission, hardlink
├── src/stack/lease/rewrite.rs               (new ~100)  substitution map (in-memory, per-root) +
│                                                        oldest-first contraction +
│                                                        acquire_rewritten_lease (one shared guard)
├── src/stack/lease/cleanup.rs               (+70)       digest/bytes sidecar removal with layer dir
│                                                        (fixes leaked .bytes); set-based membership;
│                                                        boot storage sweep (fail-closed; keep-set =
│                                                        active manifest; shares remove_layers so
│                                                        both deletion paths use one routine)
├── src/storage/fs.rs                        (+12)       syncfs helper on the storage-root fd;
│                                                        release path returns the removed set
├── src/{lib,stack/mod,stack/lease/mod,storage/mod}.rs   (+25)  wiring + exports
└── tests/unit/squash.rs (new ~400 incl. sweep) · tests/unit.rs (+1)

crates/sandbox-runtime/overlay/
├── src/kernel_mount.rs                      (+35)       move_mountpoint, strict_unmount (no lazy
│                                                        fallback); staging reuses the production
│                                                        fsconfig builder unchanged — no real-path
│                                                        mode, no second mount path
├── src/lib.rs                               (+3)        exports
└── tests/unit/kernel_mount.rs               (+40)

crates/sandbox-runtime/namespace-execution/
├── src/quiesce.rs                           (new ~200)  all-task holder-scope quiesce: cgroup ∪
│                                                        /proc ns-scan ∪ allowlist discovery,
│                                                        SIGSTOP, poll-stopped ≤ freeze budget,
│                                                        /proc pin inspection (ONE holder mountinfo
│                                                        read per session), resume-on-drop guard
├── src/engine.rs                            (+30)       engine.remount_overlay beside mount_overlay
├── src/lib.rs                               (+3)
└── tests/{quiesce.rs (new ~80), engine.rs (+25)}

crates/sandbox-runtime/namespace-process/
├── src/runner/setns/remount_overlay.rs      (new ~200)  staged switch: RemountMaskGuard narrowed to
│                                                        the build window, pre-opened dirfds,
│                                                        MS_MOVE pair, strict rollback-unmount
│                                                        (EBUSY = park, reported), two-boolean
│                                                        report (always) — no restore ladder
├── src/runner/{setns,protocol,mod}.rs       (+25)       op entry, request fields incl. fresh
│                                                        workdir path, dispatch
└── tests/unit/runner/setns.rs               (+60)

crates/sandbox-runtime/workspace/
├── src/lifecycle/remount.rs                 (new ~120)  the whole remount transaction: rewritten
│                                                        lease → freeze → runner → verify → best-
│                                                        effort persist → resume → release old lease
│                                                        (or park on EBUSY); failure rules of C5;
│                                                        mutates the existing dirs.workdir in place
├── src/service/impls/remount_workspace.rs   (new  ~40)  thin impl delegating to lifecycle
├── src/lifecycle/persistence.rs             (+50)       boot reap lives with the manager.json owner:
│                                                        destroy leftover handles/run dirs before
│                                                        the storage sweep; no schema change, no
│                                                        new session-state fields
├── src/namespace/setns_runner.rs            (+35)       NamespaceRuntime::remount_overlay
├── src/namespace/holder.rs                  (+5)        pre_exec PR_SET_PDEATHSIG(SIGKILL) —
│                                                        holders provably die with the daemon
├── src/{model,service,service/impls/mod,lib}.rs         (+30)
└── tests/unit/{remount.rs (new ~170), recover.rs (new ~60)} · tests/unit.rs (+2)

crates/sandbox-runtime/operation/
├── src/layerstack/service/impls/squash.rs   (new ~110)  daemon-local squash_layerstack op: storage
│                                                        squash + per-session sweep loop + result
│                                                        assembly (sweep lives here, not services.rs)
├── src/workspace_session/service/impls/remount_session.rs (new ~60)  per-session gate hold,
│                                                        snapshot, delegate, registry refresh
├── src/workspace_session/…                  (+25)       route run_file_op/capture/destroy through
│                                                        the per-session admission gate
├── src/command/service/core.rs              (+15/−10)   exec launch AND finalize/timeout completion
│                                                        hooks through the same gate; the global
│                                                        session_lifecycle_lock is deleted (subsumed)
├── src/services.rs                          (+10)       boot hook: reap + storage sweep, once,
│                                                        before serving
├── src/layerstack/service/{model,mod}.rs, src/workspace_session/…/mod.rs   (+35)  DTOs, exports;
│                                                        squash_layerstack registers with cli: None
│                                                        — no new OperationEntry mechanism
└── tests/layerstack_squash.rs (new ~300) · tests/support/mod.rs (+25)

crates/sandbox-manager/
├── src/operation/management/service/impls/checkpoint_squash.rs (new ~30)
│                                                        parse sandbox id, delegate to the existing
│                                                        generic forward (router/forward.rs) with
│                                                        op squash_layerstack
├── src/operation/cli_definition/management_operations.rs (+25)  checkpoint_squash CliOperationSpec
│                                                        under the existing "management" family
├── src/operation/{mod,dispatch,specs}.rs    (+10)       register
└── tests/manager_core.rs                    (+50)       catalog + forwarding tests

crates/sandbox-observability/
└── src/record.rs                            (+8)        LAYERSTACK_SQUASH, WORKSPACE_SESSION_REMOUNT,
                                                         NAMESPACE_EXEC_REMOUNT_OVERLAY

sandbox-protocol / sandbox-daemon / sandbox-gateway   (+0)
```

Totals: **10 new source files ≈ 1,290 LoC**, **≈ +420 LoC** in existing
files, **≈ 1,240 LoC** of tests → ≈ 2,950 LoC end to end. (Down from the
pre-review 12 files / ≈ 3,300 LoC: boot sweep merged into
`lease/cleanup.rs`, boot reap into `lifecycle/persistence.rs`, rewrite
relocated beside the lease registry whose lock it shares, `publish.rs`
untouched, no new manager family or operation-entry mechanism, no new
session-state fields.)

Build order: layerstack pure parts (flatten) → substitution map + rewrite →
squash transaction + boot sweep-in-cleanup → overlay move/strict helpers →
namespace-execution quiesce → namespace-process staged runner → workspace
transaction + reap-in-persistence + PDEATHSIG → operation admission gate
(subsume lifecycle lock) + op impl + sweep loop → manager CLI spec.

## Storage layout and transaction

Squash creates exactly **one temp dir per block** under `staging/`, nonce-named
via the existing `allocate_layer_dirs` path; remount creates **zero layers
and has zero required disk writes** (the post-switch `persist_handles()`
refresh is best-effort). Storage commit ends at the durable manifest rename;
remount, old-lease release, and GC are cleanup.

```text
<layer_stack_root>/
├── manifest.json                       the ONLY commit point (write_atomic: tmp → rename → dir fsync)
├── workspace.json                      binding — never touched
├── .storage-writer.lock                flock: one owning process per root
├── layers/
│   ├── B000001-base/                   never touched, never swept (B-prefix protected)
│   ├── L… (pins, singletons)           never touched
│   ├── L… (block sources)              deleted only by lease GC, never before
│   └── S000014-2a/                     ← promoted from staging by same-fs rename(2)
├── staging/
│   └── S000014-2a-<nonce>.staging/     ← the temp layer: flatten output (hardlinked/copied
│                                         winners, whiteouts, opaque markers, dir entries)
└── .layer-metadata/
    └── L….digest / L….bytes            existing publish-path sidecars (L layers only —
                                        S layers have NO sidecars: no digest, no bytes,
                                        no ledger; the substitution map is in-memory
                                        and dies with the daemon)
```

| # | Phase | Lock | Disk mutation | Crash here | Cleaner |
| --- | --- | --- | --- | --- | --- |
| 1 | plan + pin | shared, brief | none | — | plan lease drops with its guard |
| 2 | build | none (sources stay in the active manifest for the whole build — publishes only prepend) | staging trees written; durability deferred to the commit's `syncfs` | orphan staging | error path; boot sweep |
| 3 | **commit** — one exclusive critical section: recheck → promote → `syncfs` → manifest rename → release plan lease (refcount GC, returns the removed set) | exclusive | rename to `layers/S…`; one `syncfs` on the storage-root fd (covers every staging entry, whiteout, and promote rename — no per-entry fsyncs, no sidecar writes); atomic `manifest.json` replace + parent fsync; GC deletes sources no lease references | before rename: old manifest valid — in-process error path removes the promoted S dir (mirrors publish); after rename: committed | in-process error path; boot sweep |
| 4 | post-commit remount sweep | shared per session (`acquire_rewritten_lease`); exclusive per migrated session (old-lease release + GC) | none required — one best-effort `persist_handles()` refresh per migrated session (nothing reads it beyond boot reap); blocked sessions write nothing | committed squash remains valid; sessions die with the daemon | boot cleanup |

Only phases 1–3 are the storage commit path. Phase 4 improves reclaim latency
when it succeeds, but cannot change the result of a committed squash.
`replaced_layers: reclaimed|leased` is derived from what the phase-3 GC
actually deleted (the plan-lease release returns the removed set) — never
from plan-time pinning snapshots (a lease acquired between plan and commit
under the shared lock legitimately pins the run; GC sees it, a snapshot
would not). `syncfs` reports writeback errors on Linux ≥ 5.8; the supported
Docker environment runs ≥ 6.0, and the boot hook asserts the floor once.

Squash is **singleflight per layerstack root**: one in-process guard spans
plan → sweep; the `.storage-writer.lock` flock already excludes other
processes. Peak temporary storage is therefore one builder's staging.

Boot cleanup (once at daemon start, before serving):

1. **Fail closed.** Destructive sweep requires a successfully parsed
   `manifest.json` with `version ≥ 1` and a non-empty layer list. `B*` ids are
   never deleted. (No mount-boundary detector: the swept dirs contain no
   mounts, and the shared base volume is outside sweep scope, kernel-enforced
   read-only, in the keep-set, and B*-guarded.)
2. **Reap.** Holders cannot outlive the daemon (PDEATHSIG), so every persisted
   `manager.json` handle is a dead session: destroy its run dir, drop the
   handle, emit the observability record. No lease recreation, no
   orphan-liveness proof, no task resume, no per-record branching.
3. **Sweep.** Delete `staging/*`, `layers/*`, and metadata sidecars not
   referenced by the active manifest.

Metadata cleanup deletes the `.digest` and `.bytes` sidecars for the same
layer id (fixing today's leaked `.bytes`). The boot sweep and lease-release
GC share one deletion routine in `lease/cleanup.rs`, so the two paths can
never disagree on the sidecar set.

---

## B. Squash workflows

Legend: `Ln` published layer, `B` base, `S` squashed layer, `◀ bnd` =
boundary (newest layer of a live lease's manifest — "pin" is reserved for
quiesce inspection), blocks are maximal runs ≥ 2 between boundaries.

### B1. Simple — idle stack, no leases

```text
active v4                     blocks              active v5        reclaim
┌────┐
│ L3 │ ─┐                                         ┌────┐
│ L2 │  ├── one block ──▶  S1 = flatten(L3,L2,L1) │ S1 │           L3,L2,L1 deleted at
│ L1 │ ─┘                                         ├────┤           commit by the phase-3
├────┤                                            │ B  │           GC (no live lease
│ B  │                                            └────┘           references them)
└────┘
report: S1 → replaced_layers: "reclaimed"
```

No sessions ⇒ no sweep, no deferral, disk drops in one invocation.

### B2. Medium — one session pinning the whole chain

```text
active v6                blocks                active v7            sweep
┌────┐
│ L5 │ ◀ bnd ws-1        kept                  ┌────┐    ws-1 idle (no live exec):
│ L4 │ ─┐                                      │ L5 │    plain staged switch,
│ L3 │  ├─ block ─▶ S1   pinned by ws-1        │ S1 │    lease [L5,L4..L1,B] →
│ L2 │  │                                      ├────┤          [L5,S1,B]
│ L1 │ ─┘                                      │ B  │    old L4..L1 deleted on
├────┤                                         └────┘    old-lease release
│ B  │
└────┘
report: S1 → "reclaimed"   (deleted_after_migration)
```

The pin layer `L5` is kept verbatim (never copied); only the run below it
flattens. ws-1's view is bitwise unchanged; its chain drops 5 → 3 lowerdirs.

### B3. Complex — multiple pins, singleton runs, one blocked session

Sessions: ws-A leased @v9, ws-B and ws-D @v5, ws-C @v3. ws-D has an
interactive shell running (cwd-pinned).

```text
active v13 (newest→base)    boundary          block/pinning              active v14
┌─────┐
│ L12 │                      —                 ┌ singleton? no: ┐      ┌─────┐
│ L11 │ ─┐                                     │ no live lease  │      │ L12 │
│ L10 │ ─┴─────────────────────────▶ S1        │ references run │      │ S1  │
├─────┤                                        └────────────────┘      ├─────┤
│ L9  │ ◀ bnd ws-A          boundary           kept                    │ L9  │
│ L8  │ ─┐                                                             ├─────┤
│ L7  │  ├─────────────────────────▶ S2        { ws-A }                │ S2  │
│ L6  │ ─┘                                                             ├─────┤
├─────┤                                                                │ L5  │
│ L5  │ ◀ bnd ws-B, ws-D    boundary           kept                    │ L4  │
│ L4  │                      singleton run     kept (1 layer < 2)      │ L3  │
├─────┤                                                                ├─────┤
│ L3  │ ◀ bnd ws-C          boundary           kept                    │ S3  │
│ L2  │ ─┐                                                             ├─────┤
│ L1  │  ├─────────────────────────▶ S3        { ws-A,B,C,D }          │ B   │
│ L0  │ ─┘                                                             └─────┘
├─────┤                                                                14 → 9 layers
│ B   │
└─────┘

map:     S1→[L11,L10]  S2→[L8,L7,L6]  S3→[L2,L1,L0]   (in-memory)
commit:  L11,L10 deleted by the phase-3 GC (no live lease references them)

sweep:
  ws-A [L9 L8 L7 L6 L5 L4 L3 L2 L1 L0 B] ── S2 ✓, S3 ✓ ──▶ [L9 S2 L5 L4 L3 S3 B]   migrated
  ws-B [L5 L4 L3 L2 L1 L0 B]             ── S3 ✓        ──▶ [L5 L4 L3 S3 B]         migrated
  ws-C [L3 L2 L1 L0 B]                   ── S3 ✓        ──▶ [L3 S3 B]               migrated
  ws-D shell frozen → pinned:cwd_pinned_workspace → SIGCONT, lease untouched        leased

reclaim cascade:      L11 L10 │ L8 L7 L6 │ L2 L1 L0
  commit               DELETED│ pinned:A │ pinned:A,B,C,D
  ws-A migrates               │ DELETED  │ pinned:B,C,D
  ws-B migrates               │          │ pinned:C,D
  ws-C migrates               │          │ pinned:D   ← report: S3 "leased"
  ws-D shell exits / next squash / destroy            → DELETED
```

Every substitution is fully-inside or fully-disjoint per lease
(never-straddle), so each rewrite either applies whole or skips whole.

### B4. Ultra-complex — two generations, generation-crossing rewrite, races, one faulty session

```text
gen-1  active v9: [L8 L7 L6 L5 L4 L3 L2 L1 B]
       ws-1 @v8 (interactive shell, cwd-pinned)  ws-2 @v4 (idle)
       pins: L8(ws-1), L4(ws-2)  →  blocks [L7 L6 L5]→Sa, [L3 L2 L1]→Sb
       commit v10: [L8 Sa L4 Sb B]
       sweep: ws-2 → [L4 Sb B] (Sb applied); L3..L1 still pinned by ws-1 → keep
              ws-1 → pinned:cwd_pinned_workspace → leased (everything it pins stays)

t1     ws-1 shell exits; user destroys ws-1 → its release frees L7 L6 L5 L3 L2 L1
       publishes land: v12 = [L11 L10 L8 Sa L4 Sb B]
       ws-3 created @v10-era manifest (pins L10…); runs a CLEAN batch cmd
       (network wait, no cwd/fd/mmap/child mount under workspace)

gen-2  pins: L10(ws-3), L4(ws-2)
       blocks: [L11] singleton→kept · [L8 Sa]→Sc (RE-SQUASH of a prior S) ·
               [Sb] singleton→kept
       mid-build race: publish L12 lands → commit recheck still passes
       (publishes only prepend); commit v13: [L12 L11 L10 Sc L4 Sb B]
       map now (in-memory, recording order):
         Sa→[L7,L6,L5]
         Sb→[L3,L2,L1]
         Sc→[L8,Sa]                      (raw run; no build-time expansion)

       sweep:
         ws-3 live protocol: rewrite [L10 L8 Sa L4 Sb B] by oldest-first
              contraction — Sa run absent, Sb run absent (already applied),
              Sc raw run [L8,Sa] ✓ → [L10 Sc L4 Sb B];
              freeze → ZERO pins → staged switch → verify → best-effort
              persist → SIGCONT → release old lease → L8, Sa deleted
                                                                    migrated (live)
         ws-2 rewrite: no map run present in [L4 Sb B] = identity → untouched
         ws-4 (hypothetical): zero pins, switch starts, but the runner dies
              between the two MS_MOVEs — report missing at/past
              first-move-success → FAULTY: reported (id, class:detail,
              lease errors), then destroyed through the ordinary destroy
              path → namespace death → lease release reclaims everything
              only it pinned                                         faulty

crash  daemon dies between promote and manifest write of some gen:
       boot keeps the old manifest, reaps every leftover session run dir
       (holders died with the daemon), and sweeps the orphan layers/S…
       dirs. A crash mid-remount needs no special record: the session is
       dead either way; the sweep keeps exactly the active manifest's layers.
```

Contraction applies map entries in recording order (oldest generation
first), so raw runs containing earlier S ids compose across generations in
one bounded pass — a lease created in gen-0 crosses gen-1 and gen-2
substitutions without any expansion step. A missing map entry degrades that
substitution to identity, never a wrong chain; `acquire_rewritten_lease`
still validates every rewritten layer alive before acquiring.

### B5. Ultra-complex, non-faulty — live protocol under stress, clean aborts only

Same world as B4 generation 2 (post-commit v13 = `[L12 L11 L10 Sc L4 Sb B]`,
map `Sc→[L8,Sa]`), but the sweep hits every hard path and still
ends with **zero faulty sessions** — "faulty" is a narrow, provable condition,
not a synonym for "something went wrong".

```text
sweep over four sessions:

ws-3  batch cmd waiting on a socket, no workspace pins        FULL LIVE PROTOCOL
      freeze: all-task stop → /proc poll → all 'T' in ~40 ms
      inspect: cwd not under workspace ✓ · root / ✓ · fds {socket:[…], /dev/null} ✓ ·
               maps {libc,…} ✓ · mountinfo shows /workspace, ns/mnt == holder ✓ → ZERO pins
      rewrite: [L10 L8 Sa L4 Sb B] → [L10 Sc L4 Sb B]
      stage NEW (fresh workdir) → remask ✓ → probe ✓ → MS_MOVE pair → probe ✓
      strict umount rollback ✓  ← the PROOF STEP PASSES: nobody held the old mount
      best-effort persist → SIGCONT → release old lease → L8, Sa deleted
      the command never notices: a socket fd is not a workspace object,
      absolute lookups re-resolve onto the new mount, and upperdir writes
      made before the freeze are still visible after         → migrated (live)

ws-2  rewrite is identity (no substitution-map run present in its chain)
      short-circuits before any freeze                       → untouched

ws-5  interactive PTY bash at prompt, cwd=/workspace/src
      freeze ✓ → inspect: cwd pin → SIGCONT within ~50 ms    → leased
      (pinned:cwd_pinned_workspace; the shell never observes anything;
       deliberately NO registry short-circuit — every session takes the same
       freeze → inspect path, and the stall is paid only on explicit
       checkpoint_squash invocations)

ws-6  clean batch cmd, BUT the staging mount fails ENOSPC
      (the transient commit peak B+P+F filled the disk)
      failure is BEFORE the point of no return — the old
      mount was never moved; its workdir was never reused
      → SIGCONT → session untouched                          → leased
                                                   (stage_failed:staging_mount_enospc)

next invocation, minutes later (ws-3's migration freed space; ws-5's shell
has exited):
      ws-5 → plain switch → migrated · ws-6 → live protocol → migrated
      their old runs delete; the stack converges with no retry machinery,
      no persisted sweep state, zero faulty outcomes across both runs
```

The line between B5 and B4 is one proof: B5's strict `umount(rollback)`
succeeded; B4's ws-4 never reported past a successful first move. Only that
narrow path is faulty. Everything before the first successful `MS_MOVE`
aborts clean and reports `leased`; strict-unmount EBUSY after a verified
switch parks the old mount and also reports
`leased(pinned:rollback_unmount_busy)` — resumed on NEW, both leases held,
Identity on the next run.

## CLI `checkpoint_squash` output examples

`sandbox-cli` prints the result value as **one compact JSON line on stdout**
(exit 0); faults are one `{"error":…}` line on **stderr** (exit 1).
Pretty-printed here for readability.

**B1 — simple, everything reclaimed:**

```json
{
  "manifest_version": 5,
  "squashed_blocks": [
    { "squashed_layer_id": "S000005-0000001a",
      "replaced_layer_ids": ["L000003-…", "L000002-…", "L000001-…"],
      "replaced_layers": "reclaimed" }
  ]
}
```

**Nothing to squash** (no `no_op` flag — the state speaks for itself):

```json
{
  "manifest_version": 5,
  "squashed_blocks": []
}
```

**B3 — complex, mixed reclaim and one blocked shell:**

```json
{
  "manifest_version": 14,
  "squashed_blocks": [
    { "squashed_layer_id": "S000014-2a",
      "replaced_layer_ids": ["L000011-…", "L000010-…"],
      "replaced_layers": "reclaimed" },
    { "squashed_layer_id": "S000014-2b",
      "replaced_layer_ids": ["L000008-…", "L000007-…", "L000006-…"],
      "replaced_layers": "reclaimed" },
    { "squashed_layer_id": "S000014-2c",
      "replaced_layer_ids": ["L000002-…", "L000001-…", "L000000-…"],
      "replaced_layers": "leased",
      "blocked_reasons": ["pinned:cwd_pinned_workspace"] }
  ]
}
```

The stack layout and per-layer lease counts are not repeated here: the
existing `layerstack` observability view serves both, and a count printed at
sweep end is stale the instant a session exits.

**B5 — multiple leases, distinct reasons on one block** (ws-3 migrated so it
no longer pins; ws-5 and ws-6 still do):

```json
{
  "manifest_version": 13,
  "squashed_blocks": [
    { "squashed_layer_id": "S000013-3c",
      "replaced_layer_ids": ["L000008-…", "S000010-2a"],
      "replaced_layers": "leased",
      "blocked_reasons": ["pinned:cwd_pinned_workspace",
                          "stage_failed:staging_mount_enospc"] }
  ]
}
```

**Faulty outcome** — carried in the result line itself, never
observability-only (a block whose only pinning session went faulty may
legitimately report `"reclaimed"` after the destroy; the destroyed session
still appears here):

```json
{
  "manifest_version": 13,
  "squashed_blocks": [ "…" ],
  "faulty_sessions": [
    { "session_id": "ws-4",
      "class_detail": "mount_uncertain:runner_report_missing",
      "lease_errors": [] }
  ]
}
```

**Storage commit faults — one JSON line on stderr, exit 1.** The commit
recheck is run-presence, not version equality — publishes only prepend, so a
racing publish never conflicts and squash cannot starve. The recheck is
defensive (singleflight excludes competing squashes; amend also only
prepends) and unreachable in practice, so it carries **no dedicated error
kind**: any abort — recheck, build I/O failure, flatten failure, promote
failure — surfaces as the existing kind:

```json
{"error":{"kind":"operation_failed","message":"layer-stack storage error: …","details":{}}}
```

In fault cases the manifest is untouched unless the phase-3 manifest rename
was reached; partial progress before it is never referenced state, and a
post-promote in-process failure removes the promoted S dir before returning.
After commit, reclaim failures leave old runs for boot sweep or the next
cleanup pass; they do not invalidate the manifest.

Post-commit remount errors are not storage commit faults. Before the first
successful `MS_MOVE`, report `leased(class:detail)` and keep the old lease.
At/after it, strict-unmount EBUSY after a verified switch parks the session
as `leased(pinned:rollback_unmount_busy)`; anything else is reported in
`faulty_sessions` and destroyed.

There is no `--progress` streaming for squash: manager-to-daemon progress
forwarding does not exist and this plan keeps
`sandbox-protocol`/`sandbox-daemon`/`sandbox-gateway` at +0. The two
surfaces are the result line (machine) and the three observability records
(human/trace). There is no quarantine: faulty sessions are destroyed through
the ordinary destroy path after appearing in `faulty_sessions`.

---

## C. Remount workflow — workspace + namespace shell runner

### C1. Sweep decision tree (per session, inside the squash operation)

The admission gate is **one per-session gate owned by the workspace-session
service**. It blocks exec launch, one-shot create/finalize **including the
engine completion/timeout/cancel hooks** (`finalize_one_shot` must acquire it
— the PTY deadline SIGKILL itself is benign, task death only sheds pins; the
hook's capture/destroy is what must wait), file read/write/edit, capture,
destroy, namespace runner entrypoints, and remount. Command core routes
through it; the existing global `session_lifecycle_lock` is **subsumed by
this gate and deleted** — one admission concept, no version token — and the
gate is held across the whole per-session attempt. Session-not-found at the
gate is a silent skip.

```text
                acquire per-session admission gate
                (session gone → silent skip, release nothing)
                                    │
        ┌───────────────────────────┼──────────────────────────────┐
   no observable tasks        all-task freeze+inspect          freeze/inspect finds pins,
   (discovered set ⊆          proves ZERO pins                 escape, or uncertainty
    infrastructure allowlist)      │                                │
        ▼                          ▼                                ▼
  plain staged switch         full live protocol               SIGCONT immediately,
  (same gate; no freeze)      (C2/C3 below)                    release replacement lease,
        │                          │                           lease untouched
        ▼                          ▼                                ▼
    migrated                   migrated                     "leased" + class:detail
                                                            (caught by NEXT squash run —
                                                             no retry machinery exists)
   past the point of no return:
     strict-unmount EBUSY after       ──▶ park: resume on NEW, both leases
     a verified switch                    (released at destroy),
                                          leased(pinned:rollback_unmount_busy)
     anything else, incl. missing    ──▶ faulty report + destroy
     or ambiguous runner report          (ordinary destroy path)
```

Every session takes the same freeze → inspect path — there is deliberately no
registry-based short-circuit for predictably pinned sessions; the ~50 ms stall
is paid only on explicit `checkpoint_squash` invocations, and one uniform
evidence-based pipeline beats a second classification path.

### C2. Full sequence — crate swimlanes

```text
operation squash op      workspace-session       namespace-execution   workspace crate      namespace-process runner       layerstack
──────────────────────   ─────────────────────   ───────────────────   ──────────────────   ────────────────────────────   ─────────────────
squash committed (phases 1–3)
for each session in the post-commit snapshot:
  admission ────────────▶ per-session gate
                          (session gone → skip)
  acquire_rewritten_lease ─────────────────────────────────────────────────────────────────────────────────────▶ map contraction +
                                                                                                                  validate + acquire
                                                                                                                  (one SHARED-lock guard)
  Identity → unlock gate, next session
  discover + freeze ─────────────────────▶ quiesce.rs: cgroup ∪ ns-scan ∪
                                           allowlist; SIGSTOP; poll 'T' ≤
                                           freeze budget; membership stable
  inspect ───────────────────────────────▶ /proc pins + child mounts (C4)
  (pins/escape/uncertainty → release replacement lease, SIGCONT,
   unlock, "leased" + class:detail)
  staged switch ──────────────────────────────────────────────────────────▶ setns user+mnt →
                                                                            staged switch (C3) →
                                                                            two-boolean report (ALWAYS)
  report ok → swap MountedWorkspace.snapshot + dirs.workdir (existing fields),
              best-effort persist_handles   ← nothing reads it beyond boot reap
  SIGCONT ───────────────────────────────▶ resume guard
  release old lease ───────────────────────────────────────────────────────────────────────────────────────────▶ release_lease (EXCLUSIVE)
                                                                                                                  → refcount GC
  unlock gate
  report pre-PONR abort            → release replacement lease, SIGCONT, unlock, "leased" + class:detail
  report EBUSY, switch verified    → park: SIGCONT on NEW, keep BOTH leases (released at destroy),
                                     unlock, "leased" pinned:rollback_unmount_busy
  any other post-PONR outcome /
  report missing or ambiguous      → report faulty (id, class:detail, lease errors),
                                     destroy session via the ordinary destroy path;
                                     namespace death → both leases release → refcount GC
```

### C3. The staged switch (inside the holder's mount namespace)

Preconditions:

- same-upperdir/fresh-workdir kernel gate passed in live Docker;
- the staged NEW mount is built by the **same production fsconfig builder**
  as creation, so `userxattr`/no-`index` parity holds **by construction** —
  no separate mount mode, no option-string length cliff, no lowerdir-list
  introspection (equivalence is proven by behavioral witness reads); the only
  chain cap is the kernel's `OVL_MAX_STACK` (500), and an over-limit staging
  mount fails the mount syscall itself as a clean pre-PONR `stage_failed` —
  the removed visible-options helper lacked `userxattr` (deleted files would
  resurface) and must not be copied;
- all-task quiesce proof holds, or the discovered set is only allowlisted
  infrastructure.

```text
/workspace ── overlay OLD [l4, n3, n2, n1] (hidden-path masks on top)
staging  = <scratch>/.remount-staging-<pid>-<n>
rollback = <scratch>/.remount-rollback-<pid>-<n>
workdir  = <run_dir>/work-remount-<nonce>      fresh; OLD's workdir is never reused

 1. unmask hidden daemon paths                 RemountMaskGuard, build window only
 2. mount overlay NEW [l4, S(n3..n1)] at       production fsconfig builder, unchanged;
    staging; open O_PATH dirfds on staging,    the dirfds outlive the remask
    rollback, and the workspace root
 3. restore masks                              ── fail → clean abort (pre-PONR skip;
                                                  masks provably back before any move)
 4. probe staging through its dirfd:           ── fail → cleanup; OLD mount intact →
    behavioral witness reads                      clean abort ("leased")
 ────────────── point of no return = first MS_MOVE RETURNS SUCCESS ──────────────
 5. MS_MOVE /workspace → rollback (via dirfds) (a FAILED move here = clean abort)
 6. MS_MOVE staging    → /workspace
 7. probe /workspace                           ── fail → FAULTY (tasks are frozen;
                                                  nothing observes the partial state)
                                                  → report + destroy
 8. strict umount rollback — umount2(…, 0),    ── EBUSY → PARK: report verified switch;
    NO lazy/MNT_DETACH fallback                   the old mount stays at the masked
                                                  rollback point; both leases held
                                                  until session destroy
 9. report: ALWAYS — two booleans + free-form detail (first_move_succeeded,
    mount_verified); mount_verified=true only when 2–7 ALL succeeded
```

Steps 1–8 execute while every non-allowlisted task that can observe the
holder mount namespace is frozen, and masks are restored **before** the first
move, so no lookup ever threads through the hidden-path window and mask
failure is a clean skip. Step 8 succeeding is the proof the old mount had no
residual users; EBUSY there is not failure but park. Old lease release never
runs without `mount_verified=true` and task resume — and never at all in the
park case, where it waits for namespace death at destroy.

### C4. `/proc` inspection map (read while frozen; any read error = pinned)

```text
/proc/<pid>/task/<tid>/
├── stat        "4321 (bash) T … pgrp …"   membership snapshot (scan every task;
│               state T = stopped; Z excluded; 't' (ptrace-stop) requires
│               TracerPid ∈ frozen set, else quiesce_failed)
├── ns/mnt                                 must equal the holder mnt-ns inode for EVERY
│                                          discovered task → else pinned:mount_namespace_escaped
├── cwd  → /workspace/src                  dentry ref  → pinned:cwd_pinned_workspace
├── root → /                               chroot ref  → pinned:root_pinned_workspace
├── fd/9 → /workspace/build.log            open file   → pinned:fd_pinned_workspace
│    (allowlisted-safe anon fds only: PTY /dev/pts/*, socket:[…], pipe:[…],
│     eventfd, timerfd. io_uring, fanotify, and any OTHER anon_inode ⇒ pinned)
└── maps  path = bytes after the 5th       mmap        → pinned:mapped_file_pinned_workspace
     whitespace field (paths contain       unparsable line or "(deleted)" ⇒ pinned
     spaces; never "last column")
```

Holder `mountinfo` — read **once per session, never per task** (`ns/mnt`
equality above makes every frozen task's mount table the holder's): field 5
= mountpoint (octal-escaped, e.g. `\040` = space); must show the workspace
overlay; ANY child mount under the workspace root blocks — no exemptions
(hidden-path masks are namespace-root tmpfs, not workspace children).

Blocked classes (internal decision vocabulary — the wire strings are
free-form diagnostics; the only output contract is a non-empty
`blocked_reasons` when `leased`) and example details:

| class | example details |
| --- | --- |
| `unsupported` | platform, kernel gate not proven |
| `quiesce_failed` | `freeze_failed`, `freeze_timeout`, `membership_changed`, `tracer_outside_frozen_set` |
| `pinned` | `cwd_pinned_workspace`, `root_pinned_workspace`, `fd_pinned_workspace`, `mapped_file_pinned_workspace`, `child_mount_pinned_workspace`, `mount_namespace_escaped`, `rollback_unmount_busy`, `anon_inode_io_uring` |
| `mount_uncertain` | `mountinfo_unavailable`, `mountinfo_mismatch`, `proc_read_error`, `runner_report_missing` |
| `stage_failed` | `staging_mount_enospc`, `staged_probe_mismatch`, `mask_restore_failed`, `staging_mount_<errno>` (incl. the `OVL_MAX_STACK` over-limit case) |

### C5. Failure policy

This table applies only to the post-commit remount sweep. Cleanup cannot fail
or roll back the already-committed squash. **No durable remount state
exists** — every rule below is in-process; at boot every session is dead and
handled identically by the three-step boot cleanup.

| Outcome | Rule |
| --- | --- |
| Abort before the first successful `MS_MOVE` (any cause: pins, escape, freeze budget, mask-restore failure, stage/probe failure, a failed first move) | Clean skip: release replacement lease, resume tasks, report `leased(class:detail)`. Session untouched — its workdir was never reused. |
| Switch verified end-to-end (strict unmount succeeded) | Swap `MountedWorkspace` snapshot + `dirs.workdir` (existing fields), best-effort `persist_handles()`, resume tasks, release old lease (exclusive) → refcount GC. Persist failure changes nothing — nothing reads the file beyond boot reap. |
| Strict-unmount EBUSY after a verified switch | Park: resume **immediately** on the NEW mount; keep BOTH leases in memory, released at session destroy (the old superblock is alive and reading lowerdirs — releasing its lease would delete layer dirs in use); report `leased(pinned:rollback_unmount_busy)`. The next squash sees Identity — no retry loop. |
| Any other post-PONR failure — or runner report missing/ambiguous at/past first-move-success | Report faulty (session id, `class:detail`, lease-release errors), then destroy through the ordinary destroy path. Namespace death is the unmount proof; both leases release after it. Tasks stayed frozen, so nothing observed the partial state. |
| Daemon crash at any point | The session died with the daemon (PDEATHSIG). Boot: reap run dir + handle, sweep to the active manifest. No remount-specific branching. |

Faulty reporting is not optional. The result JSON's `faulty_sessions` must
include the workspace session id, `class:detail`, and lease-release errors —
never observability-only, and no byte totals (byte accounting stays with the
observability view). Publishing or capturing uncertain upperdir state is not
allowed; there is no quarantine — the report is the record.

### C6. Namespace shell runner specifics

- Process-group plumbing already exists end to end and is useful as a seed:
  the shell runner installs every command in its own group via `setpgid(0,0)`
  before exec (`namespace-process/runner/shell_exec.rs`), the PTY records it,
  and `NamespaceExecution::pgid()` exposes it
  (`namespace-execution/execution.rs`). Quiesce still needs holder-scope task
  discovery (cgroup ∪ ns-scan ∪ allowlist); pgid-only freeze is not a
  correctness proof, and there is no pgid-specific blocked reason — pgid
  failures surface as `quiesce_failed` details.
- The holder and the pid-namespace init are **always** alive in the holder
  mount namespace; they and the remount runner form the infrastructure
  allowlist — exempt from freeze and from pin inspection (daemon-owned code;
  a missed infra pin surfaces as the EBUSY park, never corruption). "No
  observable tasks" means "discovered set ⊆ allowlist".
- **Interactive PTY bash** (driven via `write_command_stdin` /
  `read_command_lines`) runs with `current_dir` inside the workspace
  (`shell_exec.rs`), so frozen it is always `pinned:cwd_pinned_workspace` →
  always "leased". This is physics, not policy: MS_MOVE would leave its cwd
  dentry on the old overlay. Such sessions migrate on the first squash run
  after the shell exits, and one-shot sessions reclaim at their
  finalize-destroy anyway. They still take the uniform freeze → inspect path.
- **Batch/waiting commands** (sleeps, network waits, no cwd/fd/mmap/child
  mount under the workspace) can freeze with zero pins → live protocol applies
  and the session migrates without the command ever noticing. Current cwd
  validation keeps absolute cwd inside the workspace; do not use outside-cwd
  examples unless that validation changes.

---

## D. Space complexity — squash with vs. without remount

Notation:

| Symbol | Meaning |
| --- | --- |
| $B$ | base layer bytes |
| $P(t)$ | total bytes of published `L` layers **and prior-generation `S` layers** retained in history at time $t$ (so $F \le P$ holds across generations) |
| $F$ | flatten size — bytes of *surviving* content, $F \le P$; for rewrite-heavy workloads $F \ll P$. Staging bytes are counted in $F$ from build start (promotion is a rename) |
| $E$ | source entry count walked while flattening, including shadowed files and whiteouts |
| $U$ | Σ session upperdir bytes |
| $Q$ | sidecar/manifest temp bytes (KB-scale, ε) |
| $\Pi(t)$ | live-session retained space: old layer bytes kept for blocked/unswept sessions |
| $T_{sess}$ | lifetime of the longest-lived session; $T_{sweep}$ = sweep duration (seconds) |

Steady-state disk under $k$ long-lived sessions:

| | no squash | squash **without** remount | squash **with** remount |
| --- | --- | --- | --- |
| after squash commit | — | $B + P + F + U$ (worse than before!) | $B + P + F + U$ (same peak) |
| steady state | $B + P(t) + U$ | $B + P(t) + F + U$ — the whole $P$ stays pinned by live leases | $B + F + U + \Pi(t)$ + pins/singletons + publish tail — only sessions that pass remount shed old pins |
| after sessions end | $B + P + U'$ | $B + F + U'$ | $B + F + U'$ |
| peak duration | — | $O(T_{sess})$ | $O(T_{sweep})$ for clean sessions |
| reclaim latency | session destroy | **max session lifetime** | **seconds** (sweep); blocked: min(shell exit → next squash, destroy) |
| old-session lowerdir chain | $n{+}1$ | $n{+}1$ (unchanged — this is the killer) | clean sessions: rewritten chain length; blocked sessions: unchanged |
| extra work per squash | — | $O(E{+}F)$ flatten | $O(E{+}F)$ flatten $+ O(k)$ remount attempts, **0 bytes** copied by remount itself |

The complexity-class statement:

```text
without remount:  disk = Θ( B + history kept for the longest-lived session + F + U )
with remount:     disk = Θ( B + F + U + Π(t) + pin/singleton layers + publish tail since last squash )
```

The with-remount bound is not unconditional: a session leased after every
publish turns every layer into a pin or singleton, blocks vanish, and disk
degenerates to the no-squash Θ(B + P(t) + U). Dense pinning is the adversarial
floor of this design.

**Re-squash cost and write amplification.** Each generation's block ends at
the previous S layer (S layers are not boundaries — B4 re-squashes `[L8 Sa]`
into Sc), so flatten re-walks the full surviving content every run; with byte
copies, cumulative writes over $G$ generations would be $\Theta(G \cdot F)$.
Whole-file winners are therefore **hardlinked** from the immutable sources
(same filesystem, promote is same-fs rename), dropping per-generation cost to
$O(E)$ metadata ops plus bytes only for content that must be re-encoded
(whiteouts, opaques, partially-shadowed trees). S layers carry no `.bytes`
sidecar; the observability view sizes them by walking (self-healing cache)
and still double-counts hardlinked files in `du`-style totals — accepted and
noted there. Build writes are not individually fsynced: commit durability is
one `syncfs` on the storage-root fd before the manifest rename, so flatten
wall clock is I/O-bound, not barrier-bound.

Storage commit optimizes the active manifest immediately. The commit peak for
live-referenced runs is $B + P + F + U$ **with or without remount** — sources
cannot be deleted before commit, so the peak carries all of $P$, not just the
session-pinned subset $\Pi$; remount changes the peak's **duration**, not its
height. Fast publishes add to the new publish tail, and the run-presence
commit recheck keeps the race closed without starving. Squash is singleflight
per root, so peak temporary storage is one builder's staging.

Percentage examples below normalize **no squash** to `100%` for each workload
after subtracting common bytes. The denominator is the squash candidate run,
not total disk:

```text
space% = retained bytes for this squash candidate run / P0 * 100
```

This cuts common $B$, $U$, and layers outside the candidate run out of the
percentage, so the table shows the retained history effect directly — with the
caveat that candidate-run normalization **understates** retained space when
pins fragment the stack (tiny candidate runs, small denominators). Let $P_0$
be the candidate run's no-squash bytes, $F$ the new flattened layer bytes, and
$\Pi$ the candidate-run bytes still pinned by sessions that did not migrate.

| Case | Lease state for candidate run | Example shape | no squash | squash, no remount | squash + remount after sweep |
| --- | --- | --- | --- | --- | --- |
| same file rewritten 6 times | no live lease references run | $P_0=6s=600$ MiB, $F=s=100$ | `600` = **100%** | `100` = **17%** after GC | same; no remount needed |
| same file rewritten 6 times | live lease still references run | $P_0=600$, $F=100$ | `600` = **100%** | $P_0+F=700$ = **117%** until remount/session exit | $F+\Pi=100+0$ = **17%** if all pinning sessions migrate |
| 6 different files edited once | no live lease references run | $P_0=600$, $F=600$ | `600` = **100%** | `600` = **100%** after GC | same; byte-neutral, lowerdirs collapse |
| 6 different files edited once | live lease still references run | $P_0=600$, $F=600$ | `600` = **100%** | `1200` = **200%** until remount/session exit | `600` = **100%**; byte-neutral, lowerdirs collapse |
| create/delete temp churn | live lease still references run | $P_0=600$, surviving $F≈10$ | `600` = **100%** | `610` = **102%** until remount/session exit | `10` = **2%** |
| delete-heavy / opaque dirs | live lease still references run | $P_0=500$, surviving $F≈50$ | `500` = **100%** | `550` = **110%** until remount/session exit | `50` = **10%** |
| many small layers, distinct content | live lease still references run | $P_0=400×1$ MiB, $F=400$ | `400` = **100%**, `400` lowerdirs | `800` = **200%** until remount/session exit | `400` = **100%**, lowerdirs collapse |
| mixed live sessions | some sessions migrate, some stay pinned | $P_0=600$, $F=100$, $\Pi=300$ | `600` = **100%** | `700` = **117%** until all sessions exit | `400` = **67%**; only blocked sessions keep old pins |

So squash without remount is already enough when no live lease references the
candidate run. Remount matters only for live-referenced runs: it lets squash
reclaim old candidate-run bytes before the session exits. For append-only
distinct content, remount mainly reduces lowerdir depth and lease cleanup cost,
not bytes.

**Chain-length limit (numeric).** One cap applies everywhere: overlayfs
`OVL_MAX_STACK` = **500** lowerdirs, for creation and for the staged remount
alike — both use the same fsconfig builder (`lowerdir+` per layer), so there
is no option-string length cliff and no second limit. Workspace creation
past 500 layers fails regardless of squash; bounding the active chain still
requires actually invoking squash — there is no trigger policy. An
over-limit rewritten chain fails the staging mount syscall itself as a clean
pre-PONR `stage_failed:<errno>` and leaves the old lease intact; rewritten
chains are bounded by ≤ 2k+2 for k plan-time boundaries, so the cap is
unreachable below k ≈ 249 concurrently pinning sessions.

**Sweep time budget.** Each attempt is freeze $O(\text{procs})$ + poll +
inspection $O(\text{procs} \times \text{fds})$; a per-session **freeze budget**
(e.g. 500 ms) bounds D-state stragglers via `quiesce_failed:freeze_timeout`
and the sweep proceeds. Total sweep duration = $O(\sum \text{procs} + \sum
\text{fds})$ + $k$ staged mounts + GC. Lease-release GC membership checks must
be set-based — $O(k \cdot n \log n)$ total, not the $O(k^2 n^2)$ a
`Vec::contains` scan would cost inside the writer lock.

---

## Required tests

Unit/integration (tests prove deleted complexity is unnecessary, not expand
the design):

1. `partition_blocks_between_boundaries_and_base` — boundaries from
   `lease_newest_layers()`; singleton runs; reclaim-vs-leased classification
   comes from the commit GC result, not plan-time snapshots.
2. `flatten_matrix` — whiteout encodings (char-dev and xattr fallback both
   re-emitted correctly), opaque markers, shadowed subtrees dropped unless the
   winner, **dir-created-then-emptied survives**, file modes preserved,
   whole-file winners hardlinked, no-follow walks through malicious symlinks.
3. `commit_gc_never_deletes_layers_leased_after_plan` — a workspace lease
   acquired between plan and commit ⇒ block reports `leased`, source dirs
   survive, the new session's mount stays healthy.
4. `commit_recheck_compacts_through_racing_publish_or_aborts_cleanly` —
   continuous publish loop; commit succeeds via run-presence with
   `version = latest + 1`; no starvation; broken run ⇒ abort surfaced as the
   existing `operation_failed` (no `manifest_conflict` kind exists anywhere
   in the wire contract).
5. `squash_singleflight_per_root` — a second invocation waits or fails
   cleanly; staging names are nonce-minted; no interleaved builders.
6. `crash_and_error_paths_around_commit` — crash after promote but before
   manifest rename: restart keeps the old manifest and boot sweeps the
   orphan S dir (which has no sidecars); a *non-crash* post-promote failure
   removes the promoted S dir in-process (no orphan awaiting restart).
7. `syncfs_commit_durability` — a syscall-recording shim proves the commit
   issues exactly one `syncfs` on the storage-root fd after promote and
   before the manifest rename, then the `write_atomic` manifest fsyncs;
   simulated power-fail after commit leaves S content, whiteouts, and
   symlinks intact — equivalence with (and replacement of) the deleted
   per-entry bottom-up fsync walk.
8. `in_memory_substitutions_match_expand_then_contract` — B4's ws-2/ws-3
   shapes including the generation-crossing raw run (`Sc→[L8,Sa]`) via
   oldest-first contraction; a missing map entry ⇒ identity in a single
   bounded pass, never a wrong chain, never a hang; after a daemon restart
   no rewrite is ever attempted (no sessions exist) and the boot sweep
   reclaims by keep-set alone — proving the deleted durable ledger was
   unnecessary.
9. `admission_blocks_all_workspace_session_entrypoints` — with
   `session_lifecycle_lock` deleted, the per-session gate alone blocks exec
   launch, one-shot create/finalize **including the timeout/cancel
   completion hook firing mid-switch** (SIGKILL of the frozen pgid must not
   let finalize's capture/destroy interleave with the MS_MOVE pair), file
   ops, capture, destroy, and runner entrypoints; destroy waits until the
   attempt resolves; no deadlock under concurrent load; session-gone at the
   gate ⇒ silent skip with no leaked replacement lease.
10. `retarget_never_runs_before_mount_verification` — injected staged/visible
    probe failure ⇒ old lease manifest unchanged, replacement lease released.
11. `post_commit_remount_failure_does_not_fail_squash_commit` — freeze/stage
    failure before PONR reports `leased`, keeps the old lease, committed
    manifest intact.
12. `persist_failure_still_migrates` — verified switch + injected
    `persist_handles` failure ⇒ tasks resume on the NEW mount, old lease
    released, report `migrated`; after a subsequent daemon kill + restart,
    boot reap destroys the run dir from the stale handle with nothing
    leaked — proving the deleted keep-both-leases persist fallback was
    unnecessary. The fresh workdir reaches `manager.json` through the
    existing `dirs.workdir` field with no schema change.
13. `old_layers_not_deleted_until_refcount_zero` — a shared run pinned by a
    second lease survives the first migration.
14. `boot_cleanup_matrix` — missing/unparsable `manifest.json` ⇒ nothing
    deleted (fail closed, `B*` respected — no mount-boundary detector
    exists); with a valid manifest: leftover run dirs and handles reaped,
    then sweep keeps exactly the active manifest's layers/sidecars; the
    boot sweep and lease-release GC delete through the one shared routine,
    and lease GC removes `.digest` + `.bytes` with the layer dir
    (regression test for today's leaked `.bytes`).
15. `ebusy_park_keeps_both_leases_and_converges` — verified switch +
    strict-unmount EBUSY ⇒ session resumes on NEW holding both leases,
    report `leased(pinned:rollback_unmount_busy)`; the next squash run sees
    Identity (no second freeze, no second switch); session destroy releases
    both leases and reclaims the old run — proving the deleted restore
    ladder was unnecessary.
16. `faulty_outcome_is_reported_then_destroyed` — post-PONR failure with a
    missing runner report ⇒ `faulty_sessions` carries session id,
    `class:detail`, lease-release errors (no upperdir byte walk occurs —
    fs-op recording shim); session destroyed via the ordinary path; leases
    release only after namespace death.
17. `squash_output_contract` — result carries exactly `manifest_version` +
    `squashed_blocks{squashed_layer_id, replaced_layer_ids,
    replaced_layers, blocked_reasons}` + `faulty_sessions` (omitted when
    empty); `blocked_reasons` non-empty whenever `leased` (strings are
    free-form); empty `squashed_blocks` when nothing to do; no `layers`, no
    `leases` fields — the observability view serves both.
18. `checkpoint_squash_manager_cli_forwards_to_runtime` — manager catalog
    exposes `checkpoint_squash --sandbox-id` under the existing
    `"management"` family; the impl delegates to the generic
    `router/forward.rs` path; runtime catalog does not expose
    `squash_layerstack` (`cli: None` — no `OperationEntry::internal`
    mechanism exists).
19. `runner_report_two_booleans_drive_policy` — kill/inject failure at each
    C3 step; every C5 outcome is reproduced as a pure function of
    `first_move_succeeded` + `mount_verified` + report presence; a missing
    report at/past first-move-success goes faulty.
20. `commit_gc_is_plan_lease_release` — instrumented idle-stack squash (B1):
    the only deletion path invoked at commit is `release_lease` on the plan
    lease (returning the removed set); with a lease acquired between plan
    and commit, the block reports `leased` and sources survive — no second
    deletion routine exists in squash.
21. `squash_commits_with_no_s_layer_sidecars` — after squash, no `.digest`/
    `.bytes`/ledger exists for the S layer; a subsequent publish on top of
    S proceeds (dedup miss is silent); the observability view sizes S by
    walking and self-heals its own cache.
22. `ultra_nonfaulty_sweep_converges` — B5 end-to-end: live migration under
    a running command, identity short-circuit, cwd-pinned clean abort,
    `stage_failed` clean abort, EBUSY park, zero faulty outcomes, full
    convergence on the following invocation with no persisted sweep state.

### Live Docker e2e (required before enabling live remount)

Three **gate tests** (G1–G3) must pass in the supported Docker environment
before live remount is enabled — any gate failure leaves squash commit-only,
with every session reported `leased(unsupported:…)` — then ten **feature
tests** (E1–E10). Every test needs **zero test-only code in `src/`**:
failures are induced naturally or by killing/observing the runner from
outside, never via in-source fault-injection flags.

Harness ground rules (apply to every test):

- **Environment preconditions, asserted once per suite, hard-fail not
  skip**: `uname -r` ≥ 5.8 (`syncfs` writeback-error reporting; supported
  kernels are ≥ 6.0); the layer-stack root's backing filesystem is not
  overlayfs (`findmnt -no FSTYPE` — overlay-on-overlay would invalidate
  every result; the Docker gateway's seeded shared base volume provides
  this); `userxattr` overlay mounts work unprivileged in the sandbox
  userns.
- **Phase observation without src hooks**: the daemon-side test observes
  runner progress by polling `/proc/<holder-pid>/mountinfo` from outside
  the namespace — the staging mount appearing = staged build done; the
  workspace root's mount ID changing = first `MS_MOVE` landed. This gives
  deterministic kill points for E7/E8/E10 with no protocol or runner
  changes.
- **Timing discipline**: tests asserting a *successful* freeze use a
  generous budget (≥ 2 s) so loaded CI cannot flake them; tests asserting
  `quiesce_failed:freeze_timeout` construct the straggler explicitly (E4)
  rather than relying on load.
- **Teardown is part of the assertion**: every test destroys its sessions,
  then asserts the lease registry is empty (`observe()`:
  `active_lease_count == 0` on every layer), no `.remount-staging-*` or
  `.remount-rollback-*` entries remain in the holder's mountinfo, and
  `staging/` is empty. Teardown uses strict unmount only — a lazy detach
  in teardown would mask exactly the leak class these tests exist to
  catch. A teardown failure fails the test loudly.
- **Witness-file convention** (G1/G2/E5): each source layer `Li` carries
  `wit/only-in-Li`, one file deleted-in-`Li+1` (whiteout winner), one dir
  created-then-emptied, and one file with a non-default mode — merged-view
  equivalence is asserted by concrete reads (presence, absence, dir shape,
  mode), never by mount-option introspection.

Gate tests:

1. `G1 same_upperdir_fresh_workdir_kernel_gate` — the one load-bearing
   kernel assumption: OLD and NEW overlays coexist on the same upperdir
   (NEW with a fresh sibling workdir) long enough for the staged switch.
   Setup: `L_old = [l2, l1]`, `L_new = [S(l2,l1)]` with equivalent merged
   witness content, one upperdir `U`, workdirs `W_old` and fresh sibling
   `W_new`, all mounts via the production fsconfig builder. Steps:
   (1) mount OLD at a workspace-shaped path; (2) write through OLD to force
   a copy-up (`cow-before`); (3) mount NEW at staging with the same `U`,
   fresh `W_new`; (4) witness-probe NEW; (5) `MS_MOVE` OLD→rollback,
   staging→root; (6) probe the visible mount; (7) `umount2(rollback, 0)`
   strict; (8) **abort leg** in a fresh tree: mount NEW at staging, unmount
   it *without* any move, then write through OLD again (`cow-after-abort`).
   Expected: every witness read exact on NEW and post-switch; step 7
   returns 0; step 8's copy-up succeeds and is durable — the assertion
   that fails if the workdir is shared. On ANY failure the suite asserts
   squash still commits and reports all sessions
   `leased(unsupported:kernel_gate_not_proven)` — the gate gates, it never
   crashes.
2. `G2 production_builder_parity_no_resurrection` — parity holds by
   construction (same builder); this proves it behaviorally: delete
   `wit/only-in-l1` through OLD (userxattr whiteout in the upperdir),
   flatten sources into `S`, build staged NEW = `[S]` + same upperdir +
   fresh workdir; the deleted file stays absent on NEW, and a whiteout
   flatten re-emitted inside `S` stays masked. Negative control: rebuild
   NEW once with a deliberately misconfigured *test-local* mount (no
   `userxattr`) and assert the file resurfaces — proving the assertion has
   teeth. No mountinfo lowerdir introspection anywhere.
3. `G3 startup_cleanup_reap_then_sweep` — two sessions (one idle, one with
   a live PTY command) plus a hand-planted orphan staging tree and an
   orphan promoted `layers/S…` dir not in the manifest. `SIGKILL` the
   daemon; poll `/proc` until holder and pid-ns init are gone (bounded
   wait — asserts PDEATHSIG, not deployment luck); restart. Expected:
   every reap record precedes the first sweep deletion record; run dirs
   and handles gone; `staging/` empty; orphan S dir gone; layers on disk
   == active manifest exactly; a new session creates and runs a command.
   Repeat with `manifest.json` unreadable: nothing deleted (fail-closed),
   daemon still serves.

Feature tests:

1. `E1 all_task_quiesce_blocks_escaped_pgid_child` — a batch command
   `setsid()`s a child holding an open fd on `/workspace/f` (outside the
   command pgid). Expected: discovery (cgroup ∪ ns-scan) finds it; it
   reaches state `T` within budget; `pinned:fd_pinned_workspace`; block
   reports `leased`; parent and child resume (state `S`/`R` asserted) and
   the command completes; old lease intact; the next squash after the
   command exits migrates the session.
2. `E2 nested_mount_namespace_blocks_remount` — `unshare -m sleep inf` with
   **zero** workspace fds; its copied vfsmount is the only pin. Expected:
   `pinned:mount_namespace_escaped`; no `MS_MOVE` ever attempted (the
   workspace root's mount ID never changes — outside observation); old
   layers retained; after killing the escapee the next run migrates. This
   exists because strict-unmount EBUSY does NOT subsume escape detection —
   a copied vfsmount pins layers without making the rollback unmount busy.
3. `E3 masks_never_observable_and_mask_failure_is_clean_skip` — (a) happy
   path: live migration while a daemon-side observer stats the hidden
   paths through `/proc/<holder>/root` in a tight loop for the whole
   sweep; no resumable task can ever see them unmasked (every
   non-allowlisted task is frozen for the entire unmask window, and
   post-resume stats return the masked view). (b) failure leg: make remask
   impossible before the moves (test-controlled read-only mask source),
   naturally forcing the narrowed `RemountMaskGuard` to fail pre-PONR.
   Expected: clean skip `leased(stage_failed:mask_restore_failed)`, masks
   verifiably restored (stat from inside the session after resume), no
   move attempted — pinning the remask-before-moves narrowing (under the
   old design this failure was post-PONR).
4. `E4 proc_pin_matrix_blocks_uncertainty` — one sub-case per pin class,
   each in its own session, one sweep: cwd inside workspace (interactive
   PTY); `chroot` into the workspace; open fd; `mmap` of
   `/workspace/a b.txt` (space in path — offset parsing, never "last
   column"); a mapping whose backing file was deleted (`(deleted)` ⇒
   pinned); a bind mount created inside the workspace by a task that has
   already **exited** — the child mount must still block via the ONE
   holder mountinfo read, proving per-task mountinfo reads are gone; an
   `io_uring` anon fd; a `ptrace` tracer outside the frozen set (`t`
   state); an unreadable `/proc` entry (any read error = pinned); a fork
   loop ⇒ `quiesce_failed:membership_changed`; a freeze straggler for
   `freeze_timeout` (a task writing into a test-controlled
   `fsfreeze`-frozen nested fs enters D-state — best-effort/optional,
   environment-sensitive). Expected: each sub-case yields its expected
   `class:detail`; every session resumes and runs a follow-up command;
   zero `MS_MOVE`s (mount IDs unchanged); old leases intact; replacement
   lease count returns to baseline (none leaked).
5. `E5 live_migration_under_running_batch_command` — B5's ws-3: a batch
   command blocked on a socket, no workspace pins, upperdir writes made
   before the freeze. Expected: migrated; the command never errors;
   post-resume reads see the pre-freeze upperdir writes; absolute-path
   lookups land on NEW; the chain shortened (witness reads, not mountinfo
   options); old source dirs deleted from disk; the block flips to
   `reclaimed` when this was the last pinning session.
6. `E6 strict_unmount_ebusy_keeps_both_leases_and_converges` — the
   SCM_RIGHTS trick: the command opens `/workspace/f`, sends the fd to
   itself over a socketpair, closes the local copy (the fd lives only in
   the socket queue, invisible to `/proc/*/fd`), and blocks. Freeze finds
   zero pins; switch verifies; strict `umount2(rollback, 0)` returns
   EBUSY. Expected: session resumes on NEW;
   `leased(pinned:rollback_unmount_busy)`; BOTH leases held (registry
   shows two); reads and copy-up work on NEW; old run NOT deleted. Second
   `checkpoint_squash`: Identity short-circuit — **no second freeze, no
   second switch** (the assertion that kills the old ladder's every-run
   retry loop). Destroy: namespace death releases both leases; the parked
   rollback mount is gone from mountinfo; layers reclaim. Pins the
   restore-ladder deletion end to end.
7. `E7 post_ponr_unverified_failure_is_faulty_destroy` — externally
   induced, no in-src injection: after the staging mount is observed, the
   test `SIGKILL`s the runner the instant the workspace root's mount ID
   changes (first move landed) — the runner dies between the moves and its
   report never arrives. Expected: missing report at/past
   first-move-success ⇒ faulty; stdout JSON carries `faulty_sessions` with
   session id, `class:detail`, lease errors (no `upperdir_bytes`, no phase
   enum); ordinary destroy runs; namespace death releases both leases; all
   layers only it pinned reclaim; the committed manifest is untouched;
   other sessions in the same sweep unaffected; exit code 0 (the squash
   committed).
8. `E8 ponr_boundary_two_boolean_report` — three externally induced
   points: (i) a *failed first move* — the rollback scratch mountpoint is
   arranged on a shared-propagation mount so `MS_MOVE` fails `EINVAL`
   (moves out of shared parents are kernel-rejected); (ii) runner killed
   after the staged build but before any mount-ID change; (iii) runner
   killed after the visible probe (mount ID changed, staging gone, report
   suppressed by the kill). Expected: (i) and (ii) are clean skips —
   `first_move_succeeded=false` or a present pre-PONR report, session
   untouched, `leased(…)`; (iii) goes faulty. All three outcomes are pure
   functions of `first_move_succeeded` + `mount_verified` + report
   presence — pinning the two-boolean report reduction.
9. `E9 staged_mount_over_ovl_max_stack_is_clean_skip` — a rewritten chain
   still exceeding `OVL_MAX_STACK` (500), e.g. 501 pinned singletons so no
   block forms below the boundaries; also workspace *creation* at 501
   layers. Expected: creation fails with the distinct documented error;
   the staged remount mount syscall itself fails (no probe, no separate
   limit detector), classified from the errno as a clean pre-PONR
   `leased(stage_failed:…)`; old lease intact; stable across repeated runs
   with no side effects — pinning the deletion of the ≈97-lowerdir
   analysis and `lowerdir_limit` probing machinery.
10. `E10 crash_matrix_recovery` — daemon `SIGKILL`ed at four
    externally-observed points: mid-freeze (tasks in `T`); mid-switch
    (between the moves, via mount-ID change); after the switch but before
    old-lease release; and between promote and manifest rename (detected
    by `layers/S…` existing while `manifest.json` is old). Expected: in
    every case the holder and pid-ns init die with the daemon
    (poll-asserted — including mid-freeze: `SIGKILL` works on stopped
    tasks, and pid-ns-init death kills the namespace); restart runs
    reap-then-sweep; disk == active manifest exactly (the old manifest for
    the pre-rename case — the crash-orphan S dir is swept); no session
    state resurrects; a fresh session plus a fresh `checkpoint_squash`
    both succeed. No remount-specific recovery branch exists to test —
    that absence is the assertion.

Explicitly not covered in e2e:

- **Commit durability (`syncfs`) under power failure** — not e2e-testable
  in Docker (a container kill does not drop the page cache); covered at
  unit/integration level by test 7 with a syscall-recording shim; a
  dm-flakey rig is out of scope. The e2e suite asserts only the
  kernel-version floor.
- **`manager.json` persist failure** — the fallback was deleted from the
  design; covered by unit test 12 plus E10's crash matrix.
