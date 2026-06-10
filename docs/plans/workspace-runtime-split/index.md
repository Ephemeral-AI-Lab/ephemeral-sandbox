# Workspace Runtime Split: Tool-Call-Centric Crates

Status: Proposed (round 2 — storage consolidated, eos-cas dissolved)
Date: 2026-06-11
Owner: sandbox/crates
Scope: replace `eos-workspace-runtime` (and the daemon glue that props it up)
with tool-call-centric crates — `eos-command-ops`, `eos-file-ops`,
`eos-ephemeral-workspace`, `eos-isolated-workspace`, `eos-command-session` —
while consolidating the storage floor: `eos-occ` merges into `eos-layerstack`,
`eos-cas` is dissolved, and no gateway crate is added. The isolated-workspace
JSONL audit pipeline is **dropped during the migration, not carried over**
(§2.6); the daemon's transport-level audit ring is a separate subsystem and
stays untouched.

The driving rule: **a workspace is an overlay that operations are performed
on — never a thing that runs commands.** Tool families own lifecycle and
decide what happens to upperdir changes; one storage engine owns durable state
and write admission; workspaces own only overlay state.

## 1. Diagnosis: what is mixed today

`eos-workspace-runtime` (~8,000 LOC) plus the daemon glue that completes it
(~3,100 LOC across `eos-daemon/src/{workspace,occ,overlay}`) interleave four
concepts; the storage floor adds two more structural smells:

| Concept | Where it lives today | Why it is wrong |
| --- | --- | --- |
| Command preparation/finalization | `ephemeral/command.rs` (329 LOC) and `isolated/command.rs` (292 LOC), ~85% structurally identical | Workspaces build `RunRequest`s, session dirs, and metadata — workspaces act as command containers. The duplication exists *because* the boundary is wrong. |
| File-op semantics | `contract/file_ops.rs` (409 LOC of implementation, not DTOs) + identical 55-LOC `ops.rs` wrappers in both modes + 388-LOC `eos-daemon/src/workspace/files/ports.rs` | One tool family smeared across three layers and two crates, behind three traits. |
| Storage access | Daemon-implemented ports (`WorkspaceRunHostPorts` god-port, `WorkspacePublisherPort`, `LayerStackSnapshotPort`); per-root OCC cache in `eos-daemon/src/occ/service_cache.rs` | An ephemeral publish traverses **two stacked ports** to reach one `apply_changeset` call. "Ephemeral" file writes never touch an overlay — they are direct fast-changes mislabeled as workspace behavior. |
| Isolation environment | `isolated/{network,session}` in the runtime crate, but holder spawn/mount/cgroup behind `NamespaceRuntimePort` implemented back in `eos-daemon/src/workspace/isolated/runtime.rs` | One subsystem split across two crates by a port that exists only to avoid admitting where the code belongs. |
| Isolated audit pipeline | `isolated/audit.rs`; the `A: AuditSink` generic threaded through `session/{lifecycle,gc,capacity,persistence}.rs`; `take_isolated_audit` smuggling JSON through `WorkspaceCommandOutcome.metadata`; fabricated payloads (`exit_code: 0`, fake `phases_ms`) in `files/ports.rs:363-388` | Audit rides inside command outcomes, crosses two crates through three indirections, and parts of the payload are fabricated. **Dropped entirely** — not redesigned, deleted. |
| Storage floor split the wrong way | `eos-occ` (2,031 LOC) exists apart from `eos-layerstack` (3,175 LOC) yet contains an `eos_occ::layerstack` bridge module, and its `CommitTransactionPort` has exactly one implementation; `eos-cas` (748 LOC) hosts both the manifest/layer data model and the ns-runner protocol DTOs — two unrelated vocabularies sharing a misnamed crate; `eos-plugin` declares an `eos-occ` dependency its sources never use | Speculative generality (a one-impl cross-crate port), a half-merged bridge, a grab-bag floor crate, and a stale edge. |

Eight workspace-runtime port traits exist today; all eight die, plus
`CommitTransactionPort` once OCC lives inside the layer stack. One new trait
(`FileBackend`, owned by `eos-file-ops`) remains.

## 2. Target architecture

### 2.1 Crate map

```
                  ┌──────────────────────────────────────────────────┐
                  │ eos-daemon  (transport, dispatch, plugins,       │
                  │ checkpoint, audit ring, composition root)        │
                  └────┬──────────────┬──────────────┬───────────────┘
                       │              │              │ enter/exit/status
              ┌────────▼───────┐  ┌───▼──────────┐   │
 command  →   │ eos-command-ops│  │ eos-file-ops │ ← │  file tools
 tools        │ CommandRegistry│  │ FileBackend: │   │
              │ CommandId →    │  │  Direct      │   │
              │ {pty, ws bind} │  │  | Isolated  │   │
              └─┬───┬────┬───┬─┘  └──┬───────┬───┘   │
                │   │    │   │       │       │       │
   ┌────────────▼─┐ │    │   │       │   ┌───▼───────▼─────────┐
   │ eos-command- │ │    │   │       │   │ eos-isolated-       │
   │ session (PTY/│ │    │   │       │   │ workspace           │
   │ process)     │ │    │   │       │   │ sessions, ns+net+   │
   └──────┬───────┘ │    │   │       │   │ cgroup env, view    │
          │  ┌──────▼───────▼──┐     │   └──────────┬──────────┘
          │  │ eos-ephemeral-  │  ┌──▼──────────────▼──────────┐
          │  │ workspace       │  │ eos-layerstack             │
          │  │ alloc → mount-  │  │ THE durable root-state     │
          │  │ plan → capture  │  │ engine:                    │
          │  │ → discard       │  │  model/  manifest, layers, │
          │  └──────┬──────────┘  │          changes, snapshot │
          │         │             │  stack/  leases, reads,    │
   ┌──────▼─────────▼──┐          │          publish, squash   │
   │ eos-namespace     │          │  route/  gitignore + base- │
   │ ns children +     │          │          hash admission    │
   │ runner protocol   │          │  commit/ single-writer     │
   └──────┬────────────┘          │          queue per root    │
          │                       │  service/ per-root facade  │
   ┌──────▼────────────┐          └──────────────▲─────────────┘
   │ eos-overlay       │─────────────────────────┘
   │ mount + capture   │   (type-only edge: capture produces
   └───────────────────┘    the stack's LayerChange vocabulary)
```

The load-bearing absences:

- **`eos-isolated-workspace` has zero storage edges** — not even type imports.
  The no-publish guarantee is a compile-time fact stronger than today.
- **`eos-ephemeral-workspace` and `eos-isolated-workspace` never see a lease.**
  They receive primitives (`layer_paths: Vec<PathBuf>`, version/hash fields)
  and return captured changes; lease custody never leaves the orchestrator
  that acquired it.
- **`eos-layerstack` has no edge to any workspace or tool crate**, and nothing
  above its implementation says "occ".

### 2.2 Crate responsibilities

| Crate | Responsibility | Public surface (sketch) | Depends on | Est. LOC |
| --- | --- | --- | --- | --- |
| `eos-layerstack` (absorbs `eos-occ`, the data model from `eos-cas`, and the daemon's per-root cache/glue) | The durable root-state engine: manifests, layers, content hashes, snapshot leases, merged reads, gitignore/base-hash write admission, the per-root single-writer commit queue, squash/GC, projection. | `service::for_root(&Path) -> RootState`; `RootState::{acquire_snapshot -> Snapshot, release_lease, read_latest, commit_direct(changes, base), publish_capture(&Snapshot, changes), project_to(dest)}`; `model::{Manifest, LayerPath, LayerChange, Snapshot}` | (leaf) | ~5,300 (3,175 + 2,031 + model + glue − bridge/port/dup) |
| `eos-overlay` | Overlayfs mechanics: kernel mounts, writable-dir layout, upperdir capture into the stack's `LayerChange` vocabulary. | unchanged + absorbs shared dir-alloc helpers | eos-layerstack (model types only) | ~1,150 |
| `eos-namespace` (absorbs the runner-protocol DTOs from `eos-cas`) | Single-threaded namespace children (`holder`, `runner`) **and** the protocol they execute: `RunRequest`, `RunMode`, `NsFds`, `ToolCall`, … | unchanged + `protocol::*` | eos-overlay | ~3,200 |
| `eos-command-session` | Policy-free PTY/process substrate: spawn under PTY, stdin, progress tail, transcript, signal/reap, current-exe ns-runner spawn. | `CommandSession::{spawn, write_stdin, read_progress, cancel, reap}` | eos-config, eos-namespace (protocol), rustix/nix/tokio (linux) | ~1,200 |
| `eos-ephemeral-workspace` | A per-operation overlay transaction. | `EphemeralWorkspace::{create(scratch_root, layer_paths), mount_plan(), capture() -> Vec<LayerChange> + stats, discard()}` (RAII guard) | eos-overlay | ~350 |
| `eos-isolated-workspace` | The persistent private workspace subsystem: caller-keyed session registry (TTL/caps/GC/persistence) and the namespace holder + veth/nftables/DNS/cgroup env (absorbed from the daemon), exposing the two read surfaces other crates consume. | `IsolatedSessions::{enter(caller, layer_paths, version, hash, ResourceCaps) -> (IsolatedWorkspaceId, …), exit -> ExitedWorkspace, status, list_open, binding(id) -> CommandBinding{ns_fds, dirs}, view(id) -> IsolatedView}`; `IsolatedView::{read (upper-first→merged), write_upper}` | eos-overlay, rtnetlink/netlink-sys/nix/rustix (linux) | ~2,500 |
| `eos-command-ops` | The command-session tool family and its lifecycle policy: owns the `CommandId → {pty session, bound workspace}` registry and decides publish (ephemeral) vs retain (isolated) at settle; holds leases for session lifetimes. | `CommandOps::{exec_command(req, ExecTarget), write_stdin, read_command_progress, cancel, collect_completed, count, sweep_expired, cleanup_caller}`; `enum ExecTarget { Ephemeral{root, scratch_root}, Isolated{caller, workspace} }` | eos-command-session, eos-ephemeral-workspace, eos-isolated-workspace, eos-layerstack, eos-namespace (protocol) | ~1,300 |
| `eos-file-ops` | The file tool family: read/write/edit semantics (size caps, base-content conflict detection, search/replace) over one backend trait. | `trait FileBackend { read, base, apply }`; `DirectBackend(RootState)`, `IsolatedBackend(IsolatedView)`; `read_file/write_file/edit_file<B>` + DTOs | eos-layerstack, eos-isolated-workspace | ~600 |

Deleted crates: `eos-workspace-runtime`, `eos-occ` (merged), `eos-cas`
(dissolved). No `eos-store`: the gateway is `eos-layerstack::service`, because
a front door to the layer stack *is* layer-stack responsibility — naming
problem dissolved with the crate.

`eos-cas` dissolution map: manifest/layer/change model + hashing invariants →
`eos-layerstack::model` (AV-1c byte-identity is a durability concern); runner
protocol DTOs → `eos-namespace::protocol` (it executes them); typed ids →
their owning registries (`CommandId` in command-ops, `IsolatedWorkspaceId` in
isolated-workspace, `LeaseId` in layerstack). `caller_id` crosses crate
boundaries as an opaque `&str` — it is daemon-issued, has no behavior below
the daemon, and a shared newtype would force back a floor crate (this is the
documented boundary reason).

`eos-daemon` keeps transport, auth, dispatch, plugins, checkpoint, the audit
ring, and a composition root that constructs `IsolatedSessions` and
`CommandOps` once and hands them to thin handlers. Response-shaping telemetry
(`/proc` + cgroup `base_timings`) stays in the daemon wire layer. The stringly
`WorkspaceTimings` maps stop threading through internal APIs; timing keys are
assembled at the wire layer only.

### 2.3 Reading the latest state (single answer for files, commands, plugins)

"Latest" is always resolved through the **active manifest under the storage
lock** — never by scanning directories. Materialization is lazy (kernel
overlay) or explicit (`project`), and any view that outlives a single call is
pinned by a lease. Five flows, all fronted by `eos-layerstack::service`:

| Consumer | Flow | Materializes? | Lease? |
| --- | --- | --- | --- |
| `read_file`, write-base reads | `RootState::read_latest(path)` → `MergedView` walks active-manifest layers newest→oldest, first hit wins | no — O(#layers) lookups | no — one-shot under the shared storage lock |
| Command session (full live tree) | `acquire_snapshot()` → `Snapshot{layer_paths}` → kernel overlayfs mount (lowerdirs = frozen layer paths) inside the runner/holder namespace | lazily, by the kernel | yes — held by command-ops (ephemeral: per command; isolated: per session) |
| Plugin service (long-lived, read-mostly) | daemon-driven refresh protocol (`eos-plugin/src/refresh.rs`): `PrepareRefresh{target_manifest_key}` → `Quiesce` → `SwapWorkspace{layer_paths, manifest_key}` → `NotifyRefresh{changed_paths\|full_resync}` → `Resume`; the daemon sources `layer_paths` from `acquire_snapshot` and is authoritative for freshness — a stale service must fail loud ("plugin projection stale"), never answer silently | per protocol (service-local view) | yes — daemon pins the swapped-in snapshot until the next swap |
| Checkpoint worktree / no-overlay fallback | `MergedView::project(dest, lease.manifest)` — full copy render (`checkpoint/commit.rs:255`, mode `"projection"`); also used internally by squash staging | yes — the only true materialization | yes, for the projection duration |
| Flatten-back (`commit_to_workspace`) | project active manifest → replace real workspace contents → rebuild base; takes the exclusive lock and is **blocked while any lease is active** | yes | n/a (exclusive) |

Plugin **writes** keep routing through the PPC callback
(`daemon.occ.apply_changeset` → `RootState::commit_direct`) — same per-root
single writer, no second entry point. Plugins get no storage dependency;
`eos-plugin`'s declared-but-unused `eos-occ` edge is dropped.

### 2.4 Lifecycle flows

`exec_command`, ephemeral (default) target — the overlay is a transaction
owned by the tool:

```
eos-daemon          eos-command-ops          eos-layerstack    eos-ephemeral-ws   eos-command-session
 op_exec_command ──► exec_command(Ephemeral)
                       ├─ service.acquire_snapshot ──► Snapshot{lease_id, layer_paths}
                       ├─ EphemeralWorkspace::create(scratch, snapshot.layer_paths) ──► dirs
                       ├─ build RunRequest{FreshNs, mount_plan}    (runner child mounts overlay)
                       ├─ spawn pty ───────────────────────────────────────────────► session
                       ├─ registry: CommandId → {session, Ephemeral(ws)}
                       │    … write_stdin / read_command_progress / cancel hit registry only …
                       └─ settle (see §2.5):
                            ├─ ws.capture() ──► Vec<LayerChange> + stats
                            ├─ service.publish_capture(&snapshot, changes) ──► single writer
                            └─ ws.discard(); service.release_lease; completion queue
```

`exec_command`, isolated target — same registry, different binding and settle:

```
 op_exec_command ──► exec_command(Isolated{caller, ws_id})
                       ├─ isolated.binding(ws_id) ──► {ns_fds, scratch dirs}
                       ├─ build RunRequest{SetNs(ns_fds)}
                       ├─ spawn pty ──► session;  registry: CommandId → {session, Isolated(ref)}
                       └─ settle: registry cleanup + completion queue only — workspace retained
                          untouched; no capture, no publish, no lease release ("do nothing")
```

File tools — two backends, no overlay on the fast path:

```
write/edit (direct):    file-ops ─ read_latest base ─ conflict check ─ commit_direct ─► gated commit
read (direct):          file-ops ─ read_latest ────────────────────► merged read of active manifest
write/edit (isolated):  file-ops ─ IsolatedView.read base ─ apply ─ write_upper ─ retained
read (isolated):        file-ops ─ IsolatedView.read (upperdir-first, then frozen layer_paths)
```

Isolated lifecycle ops (`enter`/`exit`/`status`/`list_open`) are the isolated
workspace's own API; the daemon handler composes them with storage: `enter` =
`acquire_snapshot()` → `isolated.enter(snapshot fields…, caps)`; `exit` =
`isolated.exit(id)` → `service.release_lease(lease_id)` (the daemon recorded
the lease at enter; the workspace never held it).

### 2.5 Long-lived sessions: yield, settle, sweep

A PTY session is never bound to the RPC that started it. `yield_time_ms`
shapes only the **response**, never the lifecycle: `exec_command` spawns and
registers the session, then waits at most `yield_time_ms` (default 1000ms; it
returns earlier once output has gone quiet for `quiet_ms`, today 50ms). If the
child exited inside the window the response is the settled result; otherwise
the response is `running { command_id }` and the session keeps running in the
registry — for hours if need be (`max_session_s` backstop, default 6h).

**Settle** is the once-only post-exit path. Five triggers race to observe the
exit; the first runs settle and the registry transition guarantees
exactly-once (later callers fall through to the completion queue):

| Trigger | When it settles |
| --- | --- |
| `exec_command` yield-wait | child exits within `yield_time_ms` |
| `write_stdin` yield-wait | child exits within the request's window after stdin |
| `read_command_progress` poll | poll observes the reaped child |
| `cancel` | SIGTERM→KILL, waits `cancel_wait_ms` (500ms); caller-initiated, ephemeral discards without publishing |
| periodic reaper sweep | **the only finalizer for fire-and-forget sessions**: settles exited-but-never-polled sessions and enforces the `max_session_s` wall clock (sessions without an explicit timeout get the cap, so nothing runs forever) |

Completions park in the bounded completion queue (1024, LRU drop) and drain
via `collect_completed` — the heartbeat for clients that went away and came
back. All of this is `eos-command-ops`: the registry, the settle paths, the
sweep, and startup recovery (orphaned session metadata from a previous daemon
becomes parked `orphan_reaped` completions; old children are reclaimed by
their own runner timeout, leases by layer-stack GC). The daemon contributes
exactly two hooks: a transport timer that ticks `CommandOps::sweep_expired`
(today `transport/server.rs:180`) and a startup call to recovery (`:189`).
There is no separate "command_session_manager" — `CommandOps` *is* that
manager; a second one would split registry ownership again.

One consequence to keep visible: a long-lived ephemeral session holds its
snapshot lease for its whole lifetime, which pins layer-stack GC/squash at the
lease head. That is by design — publish needs the snapshot base to gate
against — and the sweep's wall clock is what bounds it.

### 2.6 Kill list

| Today | Fate | Replaced by |
| --- | --- | --- |
| `WorkspaceRunHostPorts` (run/ports.rs) | **deleted** | `eos-command-ops` calls `eos-layerstack::service` and workspace crates concretely; daemon wire layer splices telemetry |
| `WorkspacePublisherPort` (ephemeral/ports.rs) | **deleted** | `RootState::publish_capture` |
| `LayerStackSnapshotPort` (isolated/session/ports.rs) | **deleted** | primitives passed into `enter`; lease held and released by the caller |
| `NamespaceRuntimePort` (isolated/session/ports.rs) | **deleted** | concrete holder/net/cgroup code inside `eos-isolated-workspace` (absorbs `eos-daemon/src/workspace/isolated/{runtime,ns_runner,state}.rs`) |
| `WorkspaceFileOps`, `WorkspaceReadView`, `WorkspaceMutationSink` (contract) | **deleted** | single `FileBackend` trait owned by `eos-file-ops`, two impls |
| `AuditSink` + the whole JSONL audit pipeline | **deleted** | nothing — `AuditSink`/`JsonlAuditSink`, `IsolatedSession::record_tool_call`, `take_isolated_audit` + the audit block in outcome metadata, daemon `record_tool_call`/`record_isolated_tool_call`, and the `isolated_workspace.audit_jsonl_path` config knob all go |
| `CommitTransactionPort` + `eos_occ::layerstack` bridge | **deleted** | OCC merges into `eos-layerstack`; the commit queue calls `publish_layer` as plain module code |
| `SnapshotLease` (contract) vs layerstack `Lease` | **deleted (both as a pair)** | one lean `Snapshot` returned by `acquire_snapshot`; the lease registry keeps the full `Manifest` internally |
| `eos-cas` crate | **deleted** | dissolved per §2.2 |
| `eos-plugin` → `eos-occ` Cargo edge | **deleted** | was never used by `eos-plugin/src` |
| `WorkspaceMode` flag enum | **deleted** | typed `ExecTarget` / backend choice at the tool boundary; the registry's `BoundWorkspace` enum is the only place both arms meet |
| `OccRouteProvider` (eos-occ) | kept, internal | becomes `eos-layerstack::route` module seam |

Killing the two isolated ports and the audit sink together collapses
`IsolatedSession<S: LayerStackSnapshotPort, R: NamespaceRuntimePort, A:
AuditSink>` — and the daemon's `DaemonSession` alias
(`workspace/isolated/state.rs:18`) — into the single concrete
`IsolatedSessions` type. All three generic parameters existed only to carry
seams this plan removes.

**Out of scope, deliberately:** the daemon's transport-level audit ring
(`eos-daemon/src/audit/`, `transport/tool_call_events.rs`, `api.audit.pull`)
— op-lifecycle events emitted by dispatch and the tap the e2e harness uses to
assert storage behavior (e.g. no publish during isolated runs). It never
consumed the workspace audit pipeline and is untouched here.

## 3. Migration map (today → target)

| Today | Target |
| --- | --- |
| `eos-occ/src/**` | `eos-layerstack::{route, commit}` (bridge module and `CommitTransactionPort` deleted in the move) |
| `eos-cas/src/cas.rs` (manifest/layer model + hashes) | `eos-layerstack::model` |
| `eos-cas/src/{models,runner}.rs` (runner protocol) | `eos-namespace::protocol` |
| `eos-daemon/src/occ/{mod,service_cache}.rs`, `overlay/{mod,convert}.rs` | `eos-layerstack::service` (per-root registry constructed once by the daemon; plugins' `occ_callbacks` use the same instance — MF-1 preserved) |
| `eos-workspace-runtime/src/command_session/**` | `eos-command-session` (verbatim move) |
| `eos-workspace-runtime/src/run/{manager,registry,ports,isolated_command_handle}.rs` | `eos-command-ops` (registry rewritten around `CommandId`; ports deleted) |
| `eos-workspace-runtime/src/ephemeral/{command,ops}.rs` + `isolated/{command,ops}.rs` | folded into `eos-command-ops` (one prepare/settle path on `ExecTarget`) and `eos-file-ops`; the ~620-LOC duplicate pair is deleted |
| `eos-workspace-runtime/src/ephemeral/{dirs,types,capture,finalize,timings,error}.rs` | `eos-ephemeral-workspace` |
| `eos-workspace-runtime/src/isolated/{session/**,network/**,caps,error}.rs` | `eos-isolated-workspace` (audit generic stripped on the way) |
| `eos-workspace-runtime/src/isolated/audit.rs`, `take_isolated_audit`, audit outcome blocks, daemon `record_tool_call`/`record_isolated_tool_call`, `audit_jsonl_path` config | **dropped, not migrated** |
| `eos-workspace-runtime/src/contract/file_ops.rs` | `eos-file-ops` (DTOs + semantics + `FileBackend`) |
| `eos-workspace-runtime/src/contract/{ids,lease,mode,command,mutation,read_view,response}.rs` | dissolved: `Snapshot` → `eos-layerstack::model`; ids → owning registries; the rest into owning tool crates; numeric helpers stop being exported vocabulary |
| `eos-daemon/src/workspace/files/ports.rs` | split: storage halves → `eos-layerstack::service` / `DirectBackend`; isolated halves → `IsolatedView`; the audit recorder is dropped |
| `eos-daemon/src/workspace/isolated/{runtime,ns_runner,state}.rs` | `eos-isolated-workspace` (port impls become concrete code) |
| `eos-daemon/src/workspace/{run,files,isolated}/ops.rs`, `cancel.rs` | stay in daemon as thin arg-parse → tool-crate-call handlers |
| `eos-workspace-runtime`, `eos-occ`, `eos-cas` crates | **deleted** |

Net effect: ~14k LOC reorganized with a deletion dividend of roughly
2,500–3,000 LOC — the duplicate command pair, the identical ops wrappers, nine
trait seams and their daemon implementations, the audit pipeline, the
occ↔layerstack bridge, the `Lease`/`SnapshotLease` mirror, the `eos-cas`
crate scaffolding, and the contract grab-bag.

## 4. Staged execution plan

Each stage compiles, passes `cargo test` for touched crates, and keeps the
listed e2e suites green before the next begins. Wire ops in
`eos-api/contract/ops.json` never change.

| Stage | Work | Verify |
| --- | --- | --- |
| 1 | Merge `eos-occ` into `eos-layerstack` (`route`/`commit` modules, port + bridge deleted); add `service` module absorbing the daemon's per-root cache + glue; drop `eos-plugin`'s stale dep; daemon re-points. | `eos-occ` contention + layerstack e2e suites, direct-file contracts e2e |
| 2 | Dissolve `eos-cas`: model → `eos-layerstack::model`, runner protocol → `eos-namespace::protocol`; temporary re-exports; unify `Lease`/`SnapshotLease` into `Snapshot`. | workspace-wide `cargo check`; namespace + overlay unit tests |
| 3 | Create `eos-command-session` (verbatim module move). | command-session protocol smoke e2e |
| 4 | Create `eos-ephemeral-workspace` (`create/mount_plan/capture/discard`, primitives in). | ephemeral ops unit tests + overlay-exec e2e |
| 5 | Create `eos-isolated-workspace`; absorb daemon ns runtime + `IsolatedFilePorts` view internals; delete the two ports; drop the audit pipeline and delete the `audit.jsonl` assertions in `isolated_workspace_lifecycle.rs` (~lines 182-298) in the same change. | isolated lifecycle + cross-mode consistency e2e (test updated first — assertion removal is part of the behavior change, not a cover-up) |
| 6 | Create `eos-command-ops`: registry/manager, one prepare/settle path, delete `WorkspaceRunHostPorts` + `take_isolated_audit`; daemon `run/ops.rs` re-points. | command lifecycle, cancel, sweep, isolated command e2e; `isolated_workspace_private_no_publish.rs` keeps asserting via the daemon audit tap that no publish occurs |
| 7 | Create `eos-file-ops` (`FileBackend` + semantics); daemon `files/ops.rs` re-points; delete `files/ports.rs`. | direct-file contracts + cross-mode consistency e2e |
| 8 | Delete `eos-workspace-runtime` and all temporary re-exports; restrict `WorkspaceTimings` assembly to the daemon wire layer; full gate. | full e2e suite, workspace `cargo check`/`clippy`/`test` |

## 5. Invariants preserved

- **MF-1**: exactly one commit writer per root — the per-root registry lives
  in `eos-layerstack::service`, constructed once in the daemon; file ops,
  command settles, and plugin PPC callbacks all route through the same
  instance.
- **Isolated never publishes** — enforced by crate dependency: not even a
  type import of the storage engine.
- **Atomic capture-then-publish per ephemeral run** — `capture()` walks only
  the upperdir; `publish_capture` submits one changeset against the run's
  snapshot version with base-hash revalidation, unchanged.
- **Lease/GC barriers** — leases still bracket every overlay mount, isolated
  session, plugin snapshot swap, and projection; release stays with whoever
  acquired.
- **Single-threaded namespace children** — `eosd ns-holder`/`ns-runner` and
  the current-exe spawn protocol untouched; only the protocol types' crate
  changes.
- **Wire protocol** — op names, args, and response envelopes unchanged (the
  isolated audit payload was never wire-exposed). The only external surface
  change is the removal of the `isolated_workspace.audit_jsonl_path` config
  key and the `audit.jsonl` file it pointed at.
- **Daemon transport audit ring unchanged** — `tool_call.*` events and
  `api.audit.pull` keep working; they never depended on the dropped pipeline.

## 6. Decisions

Resolved 2026-06-11:

1. **Audit: dropped wholesale.** Isolated settle is literally "do nothing";
   isolated file writes stop recording tool calls. The daemon transport ring
   stays.
2. **`eos-command-session` is its own crate** (mechanism-crate precedent:
   `eos-overlay`, `eos-namespace`).
3. **No `eos-store`; OCC merges into `eos-layerstack`.** The gateway is
   `eos-layerstack::service`. Rationale: `eos-occ` was never reusable without
   the layer stack (its only transaction impl *is* the layer stack, via a
   bridge module it already contained), and a front door to the stack is
   stack responsibility. SRP holds at module level (`model`/`stack`/`route`/
   `commit`/`service`); the crate is one bounded context — "the durable state
   of a workspace root and its write admission".
4. **`eos-cas` dissolved** — model to the stack, protocol to the namespace
   crate, ids to their owners, `caller_id` as opaque `&str` at boundaries.
5. **Workspaces take primitives, never `Snapshot`.** Lease custody stays with
   the orchestrator; the isolated crate ends with zero storage edges.

## 7. Naming review

The storage floor was already mostly well-named (`LayerStack`, `MergedView`,
`CommitQueue`, `capture_upperdir`); the rot was concentrated in the deleted
middle layer. Principles for the new crates:

1. **Name the responsibility, not the category.** `runtime`, `Manager`,
   `Ports`, `contract`, generic `Ops`, and `store` are category words that
   invite grab-bags. (`WorkspaceRunHostPorts` is three category words in a
   row; "eos-store" died of this rule and became `eos-layerstack::service`.)
2. **The relationship must read left-to-right.** A command runs *on* a
   workspace; `WorkspaceRun` inverts that.
3. **A mode word in a function name is a missing type.**
   `prepare_ephemeral_command` / `prepare_isolated_command` exist because
   `ExecTarget` didn't.
4. **One concept, one name, every layer.** One tool is spelled four ways
   today (`read_command_progress_lines` / `sandbox.command.poll` /
   `api.v1.command.read_progress` / `read_progress`); one snapshot concept is
   `Lease` and `SnapshotLease`.
5. **Name by settle-time behavior, not connotation.** `commit_or_record`
   encodes a hidden mode branch; `finish_reaped` names the trigger, not the
   meaning.
6. **Typed ids over `String` keys** — owned by the registry that mints them.
7. **Initialisms stay inside the module that implements them.** Nothing above
   `eos-layerstack::commit` says "occ".

| Current name | Problem | Replacement |
| --- | --- | --- |
| `WorkspaceRun`, `EphemeralRun`, `IsolatedRun` | inverted relationship (rule 2) | `ActiveCommand { session, workspace: BoundWorkspace }`, `enum BoundWorkspace { Ephemeral(EphemeralWorkspace), Isolated(IsolatedBinding) }` |
| `WorkspaceRunManager`, `WorkspaceRunRegistry`, `CallerRuns` | category suffixes; "run" collides with `RunRequest`/`RunMode` | `CommandOps` (public API), `CommandRegistry`, `CallerCommands` |
| `WorkspaceRunHostPorts`, `DaemonRunHostPorts`, `host_ports.rs` | pure category naming on a god-port | deleted outright |
| `finish_reaped` | names the trigger, not the meaning | `settle` (glossary: provision → run → settle) |
| `commit_or_record` | either/or verb hiding the mode branch | `FileBackend::apply` — one verb, backend defines the durable meaning, outcome reports `published` |
| `EphemeralWorkspaceOps` / `IsolatedWorkspaceOps` | identical wrappers named by mode | deleted (`FileBackend` impls: `DirectBackend`, `IsolatedBackend`) |
| `prepare_/finalize_/discard_*_command` per mode | rule 3 | one `prepare(ExecTarget)` / `settle` path in `eos-command-ops` |
| `IsolatedCommandHandle` | not a handle — a copied binding context | `CommandBinding` (returned by `IsolatedSessions::binding`) |
| `SnapshotLease` + `Lease` | two names/types for one concept | one `Snapshot` from `acquire_snapshot` |
| `WorkspaceHandleId` | which workspace? only ever isolated | `IsolatedWorkspaceId` |
| `command_session_id: String` | stringly key; drifts vs "command id" | `CommandId` newtype, serialized as `command_session_id` (wire stable) |
| `EphemeralRunDirs`, `RunDirCleanup` | "run" again | `OverlayDirs`, `OverlayDirsGuard` |
| `LayerStackRoot` newtype vs raw `root: &Path` | one concept, two spellings | `StoreRoot`-style typed root used consistently by `service` |
| `WorkspaceMode` | flag enum driving branches | deleted (`ExecTarget` / backend types) |
| `contract` module | category name that became a grab-bag | dissolved into owning crates |
| `eos-store` (round-1 proposal) | category word; the responsibility already had an owner | `eos-layerstack::service` |
| `eos-cas` | name covers half its contents | dissolved (§2.2) |
| `apply_occ_changeset`, `occ_service_for_root` above storage | rule 7 | `RootState::{commit_direct, publish_capture}` |
| `read_command_progress_lines` / `poll` / `read_progress` | rule 4 | canonical `read_command_progress` in code; wire names kept as catalog aliases |

On `ephemeral` vs `isolated` themselves: both modes are namespace-isolated,
and the "ephemeral" one is the only one that *publishes* — the pair names the
wrong axes (lifetime, isolation) instead of the defining axis (what happens to
the upperdir at settle). More honest names would be commit/transactional vs
private/session. They stay anyway — they are wire-visible product terms
(`sandbox.isolation.*`, `isolated_workspace_id`) and the cost of re-educating
every surface exceeds the gain — but each crate's rustdoc must lead with the
settle semantics: *ephemeral = per-command overlay transaction whose changes
publish on success; isolated = persistent private overlay whose changes never
leave it.*
