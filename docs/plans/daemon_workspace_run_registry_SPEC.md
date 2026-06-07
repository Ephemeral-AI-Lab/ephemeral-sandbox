# Daemon Workspace-Run Registry — Migration SPEC

Status: proposed / discussion
Owner: sandbox (daemon substrate)
Scope: `sandbox/crates/eos-daemon`, `eos-command-session`,
`eos-ephemeral-workspace`, `eos-isolated-workspace`, `eos-workspace-api`
Related:
- `docs/plans/agent_run_local_background_supervisor_SPEC.md` for agent-core
  agent-run cancellation.
- `docs/plans/backend_server_cancellation_wiring_SPEC.md` for backend-server
  cancellation orchestration.

## 1. Why this migration

Today command sessions (PTYs) live in **one flat, daemon-global registry keyed by
session id** (tagged with `caller_id`), and ephemeral workspaces are not first-class
objects at all — they exist only 1:1 with a command session, implicitly. Isolated
workspaces are a *separate* registry. Whole-sandbox operations (cancel-all, the
"no active background work" gate, commit's lease check) must re-derive ownership
from `caller_id` and hand-partition "is this caller in isolated mode?".

Target: the daemon holds **one workspace-run registry keyed by `CallerId`**, and
**each workspace run owns its own command session(s)**:

- **ephemeral workspace run** — per caller, owns exactly **one** command session
- **isolated workspace run** — per caller, owns **many** command sessions, persistent

A caller has **at most one** run, of one kind, so a single
`HashMap<CallerId, WorkspaceRun>` (enum) holds both — the XOR is structural, not a
maintained invariant. This makes `cancel_all_workspace_runs_by_caller_id(caller)` a one-call,
self-contained teardown, `cancel_all_workspace_runs` a single iteration, and gives
the lease/enter gates an authoritative source of truth. It is the **prerequisite**
for the clean §3 sandbox-cancel flow in the cancellation spec.

## 2. Current state (verified)

| Thing | Where | Shape |
|---|---|---|
| Global command-session manager | `eos-daemon/.../command_session/mod.rs:56-58` | `static MANAGER: OnceLock<CommandSessionManager>` (singleton) |
| Command-session registry | `eos-command-session/src/registry.rs:32-34` | `sessions: Mutex<HashMap<String, Arc<CommandSession>>>` + `completed` — flat, **keyed by session id**, tagged with `caller_id` |
| `CommandSession` | `eos-command-session/src/session.rs:21` | `{ id, caller_id, command, policy (overlay finalize/publish), process, output/final/transcript paths, cancelled, output_drain_grace_ms, finalized, started_at, timeout }` |
| Per-caller queries | `manager.rs:309` (`count_by_caller`), `:336` (`cleanup_caller`) | derive ownership from `caller_id` (handle multiples today) |
| Completion / reaping | `mod.rs` (`collect_completed`, `push_completed`, `sweep_expired`) | iterate the flat registry; agent-core heartbeat drains |
| Isolated registry | `eos-isolated-workspace/.../session.rs:45` (`IsolatedSession { by_caller, handles, network, scratch_root, … }`) | per `caller_id`; `list_open_callers`, `session.exit`, `reap_orphan_resources` (gc.rs:124) |
| Isolated per-workspace handle | `eos-isolated-workspace/.../session/types.rs:33` (`WorkspaceHandle`) | lease + overlay + `ns_fds`/`holder_pid`/`veth`/`cgroup_path` |
| Isolated ↔ its sessions | isolated daemon state `active_command_sessions: HashMap<id,caller>` (`mod.rs:43`); exit → `cleanup_command_sessions_for_caller` → `command_session_manager().cleanup_caller` | isolated cleans its sessions by calling the **global** manager |
| Command-bound ephemeral workspace | `DaemonEphemeralCommandPort::prepare_context(command_session_id)` → `session_dir = scratch_root/command_session_id` (`ports/ephemeral.rs:40`) | **1:1 with a command session**; daemon creates it at session start |
| Per-op overlays (OUT OF SCOPE) | `EphemeralWorkspaceOps` (`ops/files.rs:39,59,79`), `finalize_publishable_workspace` (`plugins/overlay.rs:169`) | synchronous, per-tool-call, no PTY, torn down inside the op handler |

Key consequences:
- An ephemeral and an isolated command session sit in the **same** global manager,
  distinguished only by whether `caller_id` is in isolated mode.
- The daemon "owns the PTY/process/session registry" deliberately
  (`eos-ephemeral-workspace/.../command_session/types.rs:30`). This migration keeps
  that and **composes** the run structs in the daemon.

## 3. Target model

### 3.1 Daemon workspace state — one caller-keyed map

Replaces `OnceLock<CommandSessionManager>` (the flat session map) **and** folds in
`DaemonIsolatedState` (its `active_command_sessions` side-map is dropped):

```rust
struct WorkspaceRunRegistry {
    runs: HashMap<CallerId, WorkspaceRun>,                 // ONE map — XOR (ephemeral|isolated) is structural
    completed: HashMap<CommandSessionId, CompletedEntry>,  // completion queue, drained by the agent-core heartbeat
    layer_stack_root: PathBuf,
    config: CommandSessionConfig,
}
```

A caller has **at most one** `WorkspaceRun`. The isolated enter-gate already rejects
entering while a caller's ephemeral session is active (`mod.rs:69`), so a caller is
either ephemeral *or* isolated, never both — one map entry expresses that directly.
Session-targeted ops resolve via `runs[caller_id]` then match the session id (the
wire request carries `caller_id`).

Unchanged daemon statics: plugin state, OCC cache, audit buffer,
`invocation_registry`, config `RwLock`s.

> **Behavior change (deliberate):** today a caller may hold *multiple* concurrent
> command sessions (no per-caller cap; `exec_command` just inserts; `count_by_caller`
> / `cleanup_caller` handle multiples). The per-caller model constrains a
> non-isolated caller to **one ephemeral command session at a time**; `exec_command`
> rejects a second while one is live. Concurrent commands then require separate
> callers (subagents) or isolated mode (which permits many).

### 3.2 Workspace-run structs

The **1:1 vs 1:N** cardinality is the load-bearing invariant — `session` (singular)
vs `sessions` (map). Shared field groups are extracted and composed; the closed
two-kind set is an **enum** (not a `dyn` trait — the repo's rule: enum for a closed
set, `dyn` only for open/runtime-selected sets).

```rust
// ── eos-workspace-api: shared value objects ──
struct SnapshotLease { lease_id: String, manifest_version: i64,
                       manifest_root_hash: String, layer_paths: Vec<PathBuf> }
struct Lifecycle     { created_at: f64, last_activity: f64 }

// ── eos-daemon: the two run kinds + the enum ──
struct EphemeralWorkspaceRun {           // 1:1
    caller_id: CallerId,
    session: CommandSession,             // exactly ONE (moved in from the flat manager)
    snapshot: SnapshotLease,
    dirs: EphemeralRunDirs,              // run_dir, upperdir, workdir,
    life: Lifecycle,
}

struct IsolatedWorkspaceRun {            // 1:N
    caller_id: CallerId,
    handle_id: WorkspaceHandleId,
    sessions: HashMap<CommandSessionId, CommandSession>,   // MANY (replaces active_command_sessions)
    snapshot: SnapshotLease,
    ns: NamespaceHandle,                 // ns_fds, holder_pid, readiness_fd, control_fd, veth, cgroup_path
    dirs: IsolatedRunDirs,                  // scratch_dir, upperdir, workdir,
    life: Lifecycle,
}

enum WorkspaceRun { Ephemeral(EphemeralWorkspaceRun), Isolated(IsolatedWorkspaceRun) }

impl WorkspaceRun {
    fn caller_id(&self) -> &CallerId;
    fn command_sessions(&self) -> Vec<&CommandSession>;
    async fn cancel_workspace(&mut self, reason: &str, grace: Option<f64>);   // tear down OWN resources; never OCC-publishes
}
```

Identity is `caller_id` (the map key) — there is no separate `WorkspaceRunId`. The
inner `CommandSession`(s) keep their own `command_session_id` for session-targeted
ops. `CommandSession` stays in `eos-command-session`, re-parented (see §3.3 for the
one substantive change to it).

`exec_command` (non-isolated) → create the caller's `EphemeralWorkspaceRun` if
absent; **reject if one is already live** (the one-per-caller constraint).
`exec_command` while in isolated mode → insert a session into that caller's
`IsolatedWorkspaceRun.sessions`.

### 3.3 Teardown + the OCC rule (reap/publish split)

**Cancel must DISCARD, never OCC-publish.** Make this *structural*, not a flag check.

Today the cancel path reaps via `CommandSession::try_finalize_process`
(`session.rs:262`), which calls `finalize_with_output` → `policy.finalize_command_workspace`
→ `finalize_publishable_workspace` → **`publish_upperdir_changes` (the OCC merge)**.
That helper publishes **unconditionally** (`finalize.rs:39`); `is_cancelled` only
relabels the status string. So today a cancelled command that reaps within the grace
window **merges its overlay into the shared LayerStack** — exactly what we must avoid.

Fix by **separating substrate from policy**:
- `CommandSession::reap()` (was `try_finalize_process`) only reaps the child and
  **captures** the upperdir delta — it no longer publishes, and no longer holds a
  `policy`. (`policy`/`finalized` fields are removed from `CommandSession`.)
- The **run** decides what to do with the captured delta: **complete → publish**
  (OCC merge), **cancel → discard**. The cancel path simply never calls publish, so
  "cancel never OCC-merges" is enforced by structure.

```
EphemeralWorkspaceRun::cancel_workspace(reason, grace):          // 1 session
  1. session.cancel_process()                  SIGTERM→SIGKILL on pgid; mark cancelled; drain output
  2. session.reap()                            reap child + capture delta — DO NOT publish
  3. discard_overlay(dirs, snapshot)           remove run_dir/upperdir/workdir; release_snapshot(lease)  (NO publish_upperdir_changes)
  // shared LayerStack is persisted only by the request-level commit gate, never by cancel

IsolatedWorkspaceRun::cancel_workspace(reason, grace):           // N sessions (≈ today's session.exit)
  1. for s in sessions.values(): s.cancel_process(); s.reap()      discard each (isolated upperdir is never published, by design)
  2. kill_holder(ns.holder_pid); close ns.{ns_fds, readiness_fd, control_fd}
  3. teardown_veth(ns.veth); cgroup_rmdir(ns.cgroup_path)
  4. release_snapshot(snapshot.lease_id)
  5. discard upperdir + rmtree dirs.scratch_dir
```

**Registry methods own removal** (a run never removes itself from its parent map):

```
WorkspaceRunRegistry::cancel_all_workspace_runs_by_caller_id(caller_id, reason, grace):   // per-caller op = agent-core's one RPC (§7); caller_id == agent_run_id
  if let Some(run) = runs.get_mut(caller_id): run.cancel_workspace(reason, grace); runs.remove(caller_id)

WorkspaceRunRegistry::cancel_all_workspace_runs(reason, grace):
  for run in runs.values_mut(): run.cancel_workspace(reason, grace)
  runs.clear()
  reap_orphan_resources()                  // GC handle-less eos-iws-* veth/cgroup/scratch
  // GATE (assert no leases) + commit_to_workspace live in the cancellation spec §3
```

Normal completion stays as today (reap → **publish** → push completion); only the
cancel path takes the discard branch. The branch key is `is_cancelled()` set by
`cancel_process`.

### 3.4 Resulting file / folder structure

```
sandbox/crates/
├── eos-command-session/src/
│   ├── session.rs            MOD   CommandSession + cancel_process + reap (policy/finalize REMOVED)
│   ├── process/{signal,runner}.rs   KEEP (PTY/pgid substrate)
│   ├── output.rs response.rs request.rs   KEEP
│   ├── manager.rs            DROP  (registry/cancel/cleanup role → eos-daemon)
│   ├── registry.rs           DROP  (flat session map → eos-daemon WorkspaceRunRegistry)
│   └── lib.rs                MOD   (export CommandSession + reap; drop manager/registry)
│
├── eos-ephemeral-workspace/src/
│   ├── types.rs              MOD   keep EphemeralRunDirs; EphemeralSnapshot → SnapshotLease (moves to workspace-api)
│   ├── finalize.rs capture.rs   KEEP  publish path — called by the run on COMPLETE only
│   ├── discard.rs            NEW   discard_overlay() (remove dirs + release lease, no publish)
│   ├── command_session/      DROP  (prepare/finalize/policy folded into the daemon run + finalize/discard helpers)
│   └── ports.rs dirs.rs error.rs timings.rs   KEEP
│
├── eos-isolated-workspace/src/
│   ├── session/types.rs      MOD   WorkspaceHandle → NamespaceHandle + IsolatedDirs (per-caller indexing leaves)
│   ├── session/lifecycle.rs  MOD   enter/exit → run construct + teardown helpers (kill_holder, release_lease, …)
│   ├── session/gc.rs         KEEP  reap_orphan_resources
│   ├── network.rs caps.rs    KEEP
│   ├── session.rs            MOD   IsolatedSession.{by_caller,handles} registry role → eos-daemon; keep teardown
│   └── command_session/      DROP  (isolated command-session finalize/cleanup → the run)
│
├── eos-workspace-api/src/
│   └── lease.rs              NEW   SnapshotLease, Lifecycle (shared value objects)
│
└── eos-daemon/src/
    ├── services/
    │   ├── workspace_run/                NEW  (replaces command_session/ + isolated_workspace/)
    │   │   ├── mod.rs                     NEW  service entry + with_state(WorkspaceRunRegistry)
    │   │   ├── registry.rs                NEW  WorkspaceRunRegistry + WorkspaceRun enum
    │   │   ├── ephemeral.rs               NEW  EphemeralWorkspaceRun + cancel_workspace/complete (composes session + overlay)
    │   │   ├── isolated.rs                NEW  IsolatedWorkspaceRun + cancel_workspace (composes sessions + namespace)
    │   │   ├── cancel.rs                  NEW  cancel_all_workspace_runs_by_caller_id(caller) / cancel_all_workspace_runs
    │   │   ├── completion.rs              NEW  completed queue + sweep_expired (iterate runs)
    │   │   ├── wire.rs                    MOD  (from command_session/wire.rs) op shaping
    │   │   ├── ports/ephemeral.rs         MOD  (from command_session/ports/) DaemonEphemeralCommandPort
    │   │   └── config.rs                  KEEP (from command_session/config.rs)
    │   ├── command_session/               DROP (merged into workspace_run/)
    │   └── isolated_workspace/            DROP (merged into workspace_run/)
    ├── ops/
    │   ├── registry.rs                    MOD  op table → workspace_run handlers
    │   ├── command_sessions.rs            MOD  re-point to workspace_run (op shapes unchanged)
    │   ├── isolated.rs                    MOD  enter/exit → workspace_run (exit = cancel_all_workspace_runs_by_caller_id)
    │   └── checkpoint.rs control.rs       KEEP (commit_to_workspace, op_cancel)
    └── runtime/invocation_registry.rs     KEEP
```

**Ownership rationale:** the run structs + registry live in `eos-daemon` because a
run *composes* a `CommandSession` (eos-command-session) with overlay
(eos-ephemeral-workspace) / namespace (eos-isolated-workspace) pieces — homing them
here avoids new `workspace-crate → eos-command-session` dependency edges and matches
the existing "daemon owns the PTY registry" intent. The workspace crates stay pure
overlay/namespace logic, invoked by the daemon's run methods. `eos-command-session`
shrinks to the PTY substrate.

### Carve-out (explicitly NOT migrated)

Per-op overlays (`ops/files.rs`, `plugins/overlay.rs`) are **not** workspace runs —
no PTY, no lifetime beyond the synchronous op. They keep `EphemeralWorkspaceOps` /
`finalize_publishable_workspace` as-is and never enter the registry. Interrupting
them, if ever needed, is `op_cancel` at the invocation level.

## 4. Migration approach

**Option B — re-parent + re-key (recommended).** Keep the `CommandSession` substrate
and its reaping/signalling in `eos-command-session`; replace the flat
`CommandSessionRegistry` with the caller-keyed `WorkspaceRunRegistry`. Daemon-wide
concerns (reap, completion, count, cleanup, enter gate) operate on runs. A re-homing
of ownership + the reap/publish split, **not** a rewrite of the PTY lifecycle.

**Option C — full per-workspace substrate.** Move the completion queue and reaper
into each run. Cleanest encapsulation, but relocates the central reaper/completion
plumbing the agent-core heartbeat drains. Higher risk; not recommended unless B
proves insufficient.

The rest of this spec assumes **Option B**.

## 5. Changes by area

### 5.1 Create

| Item | Home | Purpose |
|---|---|---|
| `SnapshotLease`, `Lifecycle` | `eos-workspace-api` | shared value objects composed by both run kinds |
| `enum WorkspaceRun` + `teardown` / `command_sessions` | `eos-daemon` | the closed two-kind set (enum dispatch) |
| `EphemeralWorkspaceRun` (1 session + overlay + lease) | `eos-daemon` (composes `eos-command-session` + `eos-ephemeral-workspace`) | promote the command-bound ephemeral workspace to a first-class run |
| `IsolatedWorkspaceRun` (N sessions + namespace + lease) | `eos-daemon` (composes `eos-command-session` + `eos-isolated-workspace`) | wrap the per-caller isolated handle + its sessions |
| `WorkspaceRunRegistry { runs, completed, … }` | `eos-daemon` | the single caller-keyed registry |
| `cancel_all_workspace_runs_by_caller_id(caller)` / `cancel_all_workspace_runs` | `eos-daemon` | per-caller op (agent-core's one RPC) + the whole-sandbox gate |
| `discard_overlay()` | `eos-ephemeral-workspace` | release lease + remove dirs, no publish (the cancel branch) |

### 5.2 Re-home / change

| Current | Becomes |
|---|---|
| `CommandSessionRegistry.sessions` (flat map) | `runs: HashMap<CallerId, WorkspaceRun>`; sessions owned by their run |
| `CommandSession.{policy, finalized}` + `try_finalize_process` (reap+publish) | `CommandSession::reap` (reap + capture, no publish); publish/discard decided by the run (§3.3) |
| `count_by_caller(caller_id)` | `runs.get(caller).map(\|r\| r.command_sessions().len())` (drives the enter gate) |
| `cleanup_caller(caller_id)` | `cancel_all_workspace_runs_by_caller_id(caller)` |
| `collect_completed` / `push_completed` / `sweep_expired` | iterate `runs` → each run's sessions (completion queue stays daemon-level, Option B) |
| isolated `active_command_sessions` + `cleanup_command_sessions_for_caller` | `IsolatedWorkspaceRun.sessions` owned directly (no call back into a global manager) |
| `exec_command` handler | resolve-or-create the caller's run; ephemeral → reject if one is live; isolated → insert session |

### 5.3 Drop

| Item | Why |
|---|---|
| `static MANAGER: OnceLock<CommandSessionManager>` + `CommandSessionRegistry` | replaced by `WorkspaceRunRegistry` |
| `CommandSession.policy` coupling + flat session-id keying + caller-mode partition | publish/discard moves to the run; ownership is explicit per caller |
| `DaemonIsolatedState.active_command_sessions` side-map | isolated run owns its sessions |

### 5.4 Wire-op impact (shapes preserved)

| Op | Resolution under the registry |
|---|---|
| `op_exec_command` | resolve-or-create `runs[caller]`; ephemeral → reject if a live ephemeral run exists; isolated → insert a session |
| `op_command_write_stdin` / `op_command_read_progress` / `op_command_cancel` | `runs[caller]` → the session matching `command_session_id` |
| `op_command_collect_completed` | drain `completed` (by caller) |
| `op_command_session_count` | `0` / `1` (ephemeral) or N (isolated) for the caller — feeds the enter gate |
| `op_enter` (isolated) | reject if `runs[caller]` is a live ephemeral run |
| `op_exit` (isolated) | `cancel_all_workspace_runs_by_caller_id(caller)` |

## 6. Invariants to preserve

- **One run per caller** (`HashMap<CallerId, WorkspaceRun>`): ephemeral = 1 session,
  isolated = N. XOR is structural. Enforced at `exec_command` (reject second
  ephemeral) and the enter gate.
- **A command session belongs to exactly one run** — removes the
  ephemeral-vs-isolated `caller_id` partition entirely.
- **Substrate vs policy split**: `CommandSession` reaps; the run publishes (complete)
  or discards (cancel).
- **Cancel discards, never OCC-publishes** (§3.3): the cancel path never reaches
  `publish_upperdir_changes`; the shared LayerStack is persisted on cancel solely by
  the request-level `commit_to_workspace` gate.
- **A run never removes itself from the registry** — registry methods do (§3.3).
- **Central reaping/signalling unchanged** (Option B): `CommandSession` keeps the
  pgid/process and SIGTERM→SIGKILL.
- **Completion delivery unchanged**: the heartbeat still drains `completed`.
- **Per-op overlays untouched** (carve-out §3.4).

## 7. Agent-core cancel integration

> **Prerequisite — VERIFIED: `caller_id == agent_run_id`.** The shared sandbox tool
> helper `request_base(ctx, …)` sets `caller_id = ctx.require_agent_run_id()`
> (`eos-tools/src/tools/sandbox/lib.rs:34-44`), and isolated enter/exit pass
> `agent_run_id` directly (`enter_isolated_workspace.rs:55`,
> `exit_isolated_workspace.rs:60`). So `caller_id` is **per-agent-run**: each run
> (root, subagent) has its own caller, and cancelling one run cancels exactly its own
> workspace run — never a sibling's. The one-RPC-per-caller design below is sound.

Two cancellation layers use the daemon primitives; command-session teardown collapses
to **one RPC per agent run**, and the sandbox stage adds a request-level backstop.

```
LAYER 1 — agent-core, per agent run        (agent_run_local_background_supervisor_SPEC)
  cancel_agent_run(run):
    1. stop.request()                                       stop the loop
    2. foreground executor abort_all()                      in-flight exec_command/write_stdin FUTURES dropped
    3. cancel children                                      subagents → cancel_agent_run ; workflows → cancel_workflow
    4. cancel_all_workspace_runs_by_caller_id(agent_run_id) ← ONE daemon RPC: kills this caller's PTY(s) + tears down its run
    5. finish records

LAYER 2 — sandbox stage, per request       (backend_server_cancellation_wiring_SPEC + cancellation §3)
  cancel_all_workspace_runs()                               ← GATE/backstop: sweep leftovers, then reap_orphan + GATE + commit
```

- **Foreground tools die like normal tools — but only their agent-core *future*.**
  Aborting the in-flight `exec_command`/`write_stdin` future (step 2) just stops the
  agent from *waiting*; it does **not** kill the daemon-side PTY. The PTY for a
  foreground command lives in the daemon exactly like a backgrounded one. The
  authoritative kill for **both** fg and bg is step 4's single
  `cancel_all_workspace_runs_by_caller_id`. So there is no separate fg/bg command-session
  teardown path.
- **Background command-session cancellation is trivial in agent-core** — no
  per-session enumeration; they are just entries in the caller's run, torn down by the
  one call. **Command sessions leave the agent-core background-supervisor's *cancel*
  responsibility entirely** (the supervisor keeps subagents + workflows). *Completion
  delivery* still flows daemon→`completed`→heartbeat (routed by `caller_id`); if you
  want command sessions gone from agent-core completely, route completions by
  `caller_id` and drop the supervisor command-session category (clean follow-on).
- **The sandbox gate is defense-in-depth.** After the Layer-1 recursion has cancelled
  each caller's run, `cancel_all_workspace_runs()` sweeps any run whose per-caller
  cancel failed or was never reached (e.g., an agent run that errored before step 4),
  **then** `reap_orphan_resources` + the lease-gated `commit_to_workspace`. The sandbox
  owns its own cleanup; it does not trust agent-core finished.

## 8. Migration phases & verification

0. **`caller_id` granularity — DONE.** Verified `caller_id == agent_run_id`
   (`eos-tools/src/tools/sandbox/lib.rs:34-44`; isolated enter/exit pass
   `agent_run_id`). The §7 one-RPC-per-caller design is confirmed.
1. **Shared value objects.** Add `SnapshotLease`, `Lifecycle` to `eos-workspace-api`;
   point `EphemeralSnapshot`/isolated lease fields at them. Verify:
   `cargo check -p eos-workspace-api -p eos-ephemeral-workspace -p eos-isolated-workspace`.
2. **Reap/publish split.** `CommandSession::reap` reaps + captures (no publish);
   remove `policy`/`finalized`; route normal completion's publish through the caller.
   Verify: `cargo test -p eos-command-session` + a cancel-mid-write test asserting the
   shared LayerStack manifest is unchanged.
3. **Introduce the registry (behind the flat manager).** Add `WorkspaceRun` enum,
   `EphemeralWorkspaceRun`, `IsolatedWorkspaceRun`, `WorkspaceRunRegistry` in a new
   `services/workspace_run/`. Verify: `cargo check -p eos-daemon --all-targets`.
4. **Re-home ephemeral.** `exec_command` (non-isolated) creates the caller's
   ephemeral run (reject second); route `write_stdin`/`read_progress`/`cancel`/`count`
   through it. Verify: command-session matrix E2E.
5. **Re-home isolated.** `IsolatedWorkspaceRun` owns its sessions; drop
   `active_command_sessions` + `cleanup_command_sessions_for_caller`; `op_exit` =
   `cancel_all_workspace_runs_by_caller_id(caller)`. Verify: isolated lifecycle + enter-gate E2E.
6. **Re-point daemon concerns.** completion/sweep/heartbeat iterate `runs`. Verify:
   completion-delivery + backpressure E2E.
7. **Cancel surface.** `cancel_all_workspace_runs_by_caller_id` / `cancel_all_workspace_runs` (teardown
   in the run, removal in the registry). Verify: cancel-all E2E (no live sessions /
   leases) + the cancel-mid-write manifest-unchanged E2E.
8. **Remove the flat registry + merge services.** Delete
   `OnceLock<CommandSessionManager>` and `command_session/`/`isolated_workspace/`
   service modules (or leave thin re-pointing shims one release). Verify:
   `cargo clippy -p eos-daemon -p eos-command-session --all-targets -- -D warnings` +
   full `eos-command-session` + isolated E2E suites.

### Success criteria

- The daemon holds one `HashMap<CallerId, WorkspaceRun>`; every command session is
  owned by exactly one run (ephemeral = 1, isolated = N).
- All existing command-session and isolated wire ops behave identically (same E2E
  results), routed through the registry.
- A cancelled command never OCC-merges (cancel-mid-write manifest test passes).
- `cancel_all_workspace_runs` tears down every run with no `caller_id` partition logic.
- Per-op overlays are unchanged and never registered.

## 9. Risks & open questions

- **`caller_id` granularity — RESOLVED.** Verified `caller_id == agent_run_id`
  (`eos-tools/src/tools/sandbox/lib.rs:34-44`; isolated enter/exit pass `agent_run_id`),
  so the §7 one-RPC integration and the one-per-caller constraint are sound.
- **Finalize split (OCC merge).** Removing `policy`/`finalize` from `CommandSession`
  is the highest-churn change (its tests assume the session finalizes). Risk: a
  missed branch silently merges a cancelled command's writes. Cover with the
  cancel-mid-write manifest-unchanged test.
- **Completion queue placement (Option B vs C).** B keeps `completed` daemon-level
  (sourced by iterating runs); confirm delivery order + reported-once survive.
- **Isolated multi-session teardown ordering.** Cancel all sessions before
  namespace/lease teardown — `session.exit` already does this; confirm parity when
  sessions are owned directly.
- **Sweep/TTL reaper** must expire an ephemeral run (its one session) and individual
  isolated sessions without tearing down the persistent isolated run.
- **Service merge churn.** Merging `command_session/` + `isolated_workspace/` into
  `workspace_run/` is sizable; the shim option (phase 8) de-risks it.
