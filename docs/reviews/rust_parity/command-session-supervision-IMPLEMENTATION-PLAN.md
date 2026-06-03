# Implementation Plan — Unified PTY Command-Session Supervision, Notification & Daemon Wait Model (Rust)

**Status:** Slice 1 ✅ landed & verified · Slice 2 ✅ implemented (Linux-target compile/clippy-clean; macOS host tests pass) · Slice 3 🔶 daemon `terminate` flag landed, tool-side polish deferred.

> **Implementation progress (this pass).**
> - **Slice 1 — Supervision + notification (agent-core): COMPLETE & VERIFIED.** `CommandSessionRecord` + supervisor methods (`background/command_session.rs`), `CommandSessionSupervisorPort` (`eos-tools/ports.rs`) + `ExecutionMetadata` field, the per-request command-completion heartbeat (`background/heartbeat.rs`, adds the `eos-engine → eos-sandbox-api` edge — D2), the loop-drains-the-sink rework, the notification rules refactored into a `notifications/` module with a `NotificationRule` trait and one rule per file under `notifications/rules/` (`terminal_reminder.rs`, `tool_budget.rs`), the text-return nudge trigger (reasoning-only never nudges), `QueryContext.notifier`, and the full `entry.rs` instance-identity wiring + heartbeat lifecycle. Verified: whole agent-core workspace green (incl. an end-to-end test through real `start_request` proving the heartbeat sink and the loop notifier are the same `NotificationService`), clippy-clean, dependency-DAG frozen set updated. Committed as `b4deb1a95`.
> - **Slice 2 — Daemon sense-2 (`eos-daemon/src/command.rs`): IMPLEMENTED.** `Child`-in-session, one idempotent `try_finalize(publish)` (the two former detached finalizer threads deleted), the unified `wait_for_yield` (quiet-after-output early return) shared by `exec_command`/`write_stdin`, the `CommandSessionReaper` (timeout backstop + unpolled finalize) wired in `server.rs`, conservative `recover_orphaned_command_sessions` startup scan, the UTF-8 carry-over decode fix (pure helper, unit-tested on every host), the `CommandWorkspaceKind` enum, and the `CommandWorkspace → EphemeralCommandWorkspace` rename. **Verification:** compiles + `clippy`-clean on `aarch64-unknown-linux-musl` (the real daemon target) and the macOS host (non-Linux stubs); the pure carry-over helper is runtime-tested. The syscall-bound paths (PTY child reaping, `killpg`, lease release, OCC publish) are **compile-verified only** — they need a Linux runtime to exercise. The OCC/lease/capture finalize **bodies are byte-identical** to the prior passing daemon; only the orchestration around them changed. Orphan recovery deliberately does **not** `killpg` (pgids are not persisted; a restarted daemon could signal a reused PID).
> - **Slice 3 — Tool polish: PARTIAL.** The daemon-side `terminate: bool` teardown channel (D7) is landed in `write_stdin`. The agent-core tool-side (`WriteStdinInput.terminate`, dropping the `\x03`→cancel escalation, expanded `exec_command`/`write_stdin` descriptions, "stderr always empty" doc, `default_tool_specs.snap` re-pin) is **deferred** to avoid clobbering a parallel agent actively rewriting `eos-tools/model_tools/sandbox.rs`.
>
> **Original status:** Design finalized; not yet implemented.
**Scope:** `agent-core/` (Rust agent runtime) + `sandbox/` (Rust `eosd` daemon). **Python is left untouched** (deprecating).
**Author/context:** consolidated from the design discussion; all anchors verified against the live tree.

> This is a self-contained plan. It supersedes the earlier `docs/plans/sandbox-rust-external-migration-PHASE-3T-PTY-COMMAND-DESIGN.md` for the agent-core supervision + daemon wait/finalize model; the daemon's PTY/cursor substrate it describes is retained.

---

## 0. Locked decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Rust-only.** No Python changes. | Python is deprecating; both Rust codebases in scope. |
| D2 | **`sandbox/` stays fully independent.** agent-core pulls via `eos-sandbox-api`; **no reverse dependency, no shared types crate** — the **JSON completion map is the contract**. | One-way DAG edge `eos-engine → eos-sandbox-api → daemon`. |
| D3 | **The loop is the sole writer of conversation `messages`.** External subsystems influence the conversation only as **data through the sink**. | Encapsulation; already structurally true (verified: no external `&mut Vec<Message>`; `build_query_run_request` takes `&[Message]`; `dispatch_assistant_tools` never receives `messages`). |
| D4 (revised) | **Single notification path: everything goes through the `NotificationSink`,** drained by the loop at the top of **every** turn. There is **no "imperative direct-inject" class.** The only non-sink control-flow item is the **150% `TerminalNotSubmitted` failure** (a loop *exit*, not a notification). | The loop drains the sink every turn deterministically ⇒ the sink is already immediate; a special direct path is unnecessary. |
| D5 | **Background supervisor = per-request instance, per-task-run ownership (`agent_id`).** It pulls daemon completions via a **self-scheduled heartbeat** (~1 s adaptive, off when idle). **The loop never touches the daemon.** | Matches the existing subagent/workflow supervision; preserves isolation; `agent_id` ≈ one Task run. |
| D6 | **Daemon "sense 2": unify `exec_command` + `write_stdin`** behind one `wait_for_yield` + one idempotent `try_finalize`. The `Child` lives **in the session**; the **per-session finalizer thread is removed**; a **daemon reaper** handles unpolled exit + timeout. The isolated variant is unified via a `CommandWorkspaceKind` branch. | One wait, one finalize for both verbs; kills the `write_stdin` flat-sleep wart; removes thread-per-session. |
| D7 | **Ctrl-C decoupled (Rust-only):** `\x03` = SIGINT only (interrupt); new **`terminate: bool`** on `write_stdin` for teardown. | Fixes the REPL-kill wart; Python divergence acceptable (deprecating). |
| D8 | **No `cancel(event)` / sink retraction.** Exactly-once is achieved by (a) sense-2 inline finalize ⇒ not parked ⇒ no notification, and (b) the supervisor's `Delivered` latch (+ `write_stdin` returning a terse "already reported" when the record is already `Delivered`). | The daemon's `take`-removes covers the common case; only the narrow heartbeat-first overlap needs the latch. |

**Notification taxonomy (D4):** all three notification kinds — **budget warnings (75/100/125%)**, **`[BACKGROUND COMPLETED]` completions**, and the **terminal-submit nudge** — are *sink producers*. The terminal nudge is triggered by an assistant **text return** (see §6). The 150% failure is a loop exit.

---

## 1. System architecture

```
┌──────────────────────── agent-core process (Rust) ───────────────────────┐
│                                                                           │
│  QueryContext.messages  ◀─── SOLE WRITER ───  loop_.rs (run_query)        │
│         ▲ append_notifications (private)            │ each turn (top):     │
│         │                                           │  drain sink → inject │
│   NotificationService (sink)  ◀── notify_system ────┤  budget rules        │
│         ▲          ▲                                │  terminal nudge      │
│         │          └── heartbeat: [BACKGROUND COMPLETED]   (loop DAEMON-FREE)│
│   BackgroundTaskSupervisor ──── heartbeat task ─────┼──┐                   │
│     · command_sessions: Map<id, CommandSessionRecord>  │ collect_command_  │
│     · subagents/workflows (existing)                   │ completions()     │
│         ▲ register / recover / mark (via port)         ▼                   │
│   tools: exec_command, write_stdin  ── eos-sandbox-api transport ──┐       │
└────────────────────────────────────────────────────────────────────┼─────┘
                       one-way dep (agent-core → eos-sandbox-api)      │ JSON-RPC
┌──────────────────────── sandbox/ daemon (Rust, isolated) ───────────▼─────┐
│  CommandSessionRegistry { sessions, completed }                           │
│  CommandSession { …, child, workspace: CommandWorkspaceKind, finalized }  │
│      └─ try_finalize(publish) ◀── wait_for_yield ◀── exec_command/write_stdin (inline) │
│  CommandSessionReaper (periodic): timeout-kill + unpolled exit → try_finalize(true)    │
│  startup: recover_orphaned_command_sessions()                             │
│  runner (eos-runner) enforces per-call timeout internally (primary)       │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Daemon (`sandbox/crates/eos-daemon`) — sense 2

### 2.1 Before → after

| Aspect | **Before** | **After (sense 2)** |
|---|---|---|
| `Child` ownership | moved into a **per-session detached thread** (`command.rs:775-782`) | lives **in the session** (`Mutex<Option<Child>>`) |
| Finalize call sites | `exec_command` inline `finish(false)`; detached thread `finish(true)`; `write_stdin` reads parked map | **one** `CommandSession::try_finalize(publish)`, called inline by `exec_command`/`write_stdin`/reaper |
| `write_stdin` wait | flat `thread::sleep(yield_time_ms)` then `take` (`command.rs:1413`) | Ephemeral `wait_for_yield`: early-return on completion **or** quiet-after-output, deadline cap |
| `exec_command` wait | 5 ms busy-poll, early-return on **exit** only (`:752-763`) | same `wait_for_yield` (adds quiet-after-output) |
| Unpolled completion / timeout | detached thread blocking `child.wait()`; timeout enforced only in the **runner child**, only if supplied | **`CommandSessionReaper`** (periodic `try_wait` + timeout backstop); runner stays primary |
| Threads | **1 per live session** | **0 per session** + 1 daemon reaper |
| Ctrl-C | `\x03` → SIGINT **and** tool escalates to cancel | `\x03` → **SIGINT only**; `terminate:true` → teardown |
| PTY decode | `from_utf8_lossy` per 8 KiB read (`:1002`) splits multibyte | **carry-over buffer** across reads |
| Startup orphans | none (gap) | `recover_orphaned_command_sessions()` scan |
| Dedup | parked entry read by both `take` + `collect` ⇒ possible double | inline finalize ⇒ not parked ⇒ no double; reaper-only park |

### 2.2 Unified wait + finalize (the heart of sense 2)

```rust
// PROPOSED — shared by BOTH verbs
enum WaitOutcome { Completed(Value), Running(String) }

fn wait_for_yield(session: &Arc<CommandSession>, yield_time_ms: u64) -> WaitOutcome {
    let deadline = Instant::now() + Duration::from_millis(yield_time_ms);
    let start_off = session.output.next_byte_offset();
    let (mut last_off, mut last_change) = (start_off, Instant::now());
    loop {
        if let Some(result) = session.try_finalize(/*publish=*/false) {   // (1) completed → inline
            return WaitOutcome::Completed(result);
        }
        let off = session.output.next_byte_offset();
        if off != last_off { last_off = off; last_change = Instant::now(); }
        if off > start_off && last_change.elapsed() >= QUIET_MS {          // (2) responded + settled
            return WaitOutcome::Running(session.read_model_output(None));
        }
        if Instant::now() >= deadline {                                   // (3) cap
            return WaitOutcome::Running(session.read_model_output(None));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

impl CommandSession {
    // PROPOSED — idempotent (finalized latch); at-most-once across exec / write_stdin / reaper.
    fn try_finalize(&self, publish: bool) -> Option<Value> {
        let mut latch = lock(&self.finalized);
        if let Some(cached) = latch.as_ref() { return Some(cached.clone()); }
        let mut child = lock(&self.child);
        match child.as_mut()?.try_wait().ok()? {
            None => None,                                  // still running
            Some(status) => {
                let result = finalize_workspace(self, status);            // §2.6 kind-branch
                command_session_registry().remove(&self.id);
                if publish { command_session_registry().push_completed(/* + notification_result */); }
                *latch = Some(result.clone());
                Some(result)
            }
        }
    }
}
```

### 2.3 The two verbs become symmetric

```
exec_command(cmd, timeout, yield):              write_stdin(id, chars, terminate, yield):
  spawn child on PTY                               session = registry.get(id)? or take_completed
  build session (child IN session,                 write chars to PTY writer
    timeout_deadline = now + timeout)              if chars.contains('\x03') killpg(SIGINT)   // interrupt only
  match wait_for_yield(session, yield):            if terminate killpg(SIGTERM→SIGKILL)        // teardown
    Completed(r) => strip_session_id(r)            match wait_for_yield(session, yield):
    Running(out) => { registry.insert();             Completed(r) => r        // SAME finalize path
                      running + id + out }            Running(out) => running + out
```

### 2.4 Daemon reaper + startup recovery

```rust
// PROPOSED — sandbox/crates/eos-daemon/src/command/reaper.rs
fn command_session_reaper_sweep() {           // periodic (~50 ms), spawned in server.rs serve()
    for session in command_session_registry().live() {
        if let Some(dl) = session.timeout_deadline {            // §3 timeout backstop
            if Instant::now() > dl { terminate_command_process_group(session.pgid); }  // SIGTERM→50ms→SIGKILL
        }
        session.try_finalize(/*publish=*/true);                // unpolled exit → park + notify
    }
}
fn recover_orphaned_command_sessions() {      // once at startup, BEFORE listeners accept (server.rs ~143)
    for dir in read_dir("runtime/command-sessions/*") {
        // read metadata.json (+ final.json); killpg survivors; push_completed(orphan_reaped); rm dir
    }
}
```

### 2.5 `CommandSession` struct diff

```diff
  struct CommandSession {
      id: String, agent_id: String, command: String,
      started_at: Instant, pgid: i32,
      writer: Mutex<File>,
      output: Arc<CommandSessionOutput>,
-     reader_done: Mutex<Option<std_mpsc::Receiver<()>>>,
      cancelled: Mutex<bool>,
      interrupted: Mutex<bool>,
      model_cursor: Mutex<CommandSessionOutputCursor>,
      notification_cursor: Mutex<CommandSessionOutputCursor>,
+     child: Mutex<Option<Child>>,            // was moved into the detached thread
+     workspace: CommandWorkspaceKind,        // Ephmeral | Isolated (see §4)
+     finalized: Mutex<Option<Value>>,        // idempotency latch + cached terminal result
+     timeout_deadline: Option<Instant>,      // reaper-enforced backstop
  }
- // DELETE: CommandSessionFinalizer / IsolatedCommandSessionFinalizer structs + the thread::spawn.
+ // try_finalize() + the reaper subsume both finalizers (publish flag = parked-for-notification).
```

### 2.6 UTF-8 carry-over (`spawn_command_output_reader`, `command.rs:1002`)

```diff
- let text = String::from_utf8_lossy(&buf[..n]).into_owned();   // splits multibyte at the 8KiB edge
- output.append(text);
+ carry.extend_from_slice(&buf[..n]);
+ let valid = utf8_valid_prefix_len(&carry);                    // decode valid prefix, retain ≤3-byte tail
+ output.append(String::from_utf8_lossy(&carry[..valid]).into_owned());
+ carry.drain(..valid);                                         // flush remainder at EOF
```

---

## 3. Timeout → cancel → notification (#1)

**Current mechanism (verified):** the per-call timeout is enforced **inside the `eos-runner` child**, not the daemon — `wait_for_command_execution_scope` (`eos-runner/src/fresh_ns.rs:327-360`) polls a deadline and `killpg(SIGKILL)`s the group, returning exit `124` + status `"timed_out"` (`fresh_ns.rs:259-265`), serialized to the `--output` file (`eosd/src/main.rs:156-158,525`) and read back by the finalizer (`command.rs:1031,1047-1052`). It is enforced **only when `timeout_seconds` is supplied**; with `None` the runner — and the daemon `child.wait()` — are **unbounded** (the real gap).

**Design:**

```
exec_command(cmd, timeout)
   │  timeout_seconds → runner RunRequest  AND  CommandSession.timeout_deadline = started_at + timeout
   ▼
runner enforces internally  ──(self-kill at deadline → status "timed_out", exit 124)──┐   PRIMARY
reaper backstop (each sweep):  now > timeout_deadline && !finalized                    │   BACKSTOP
   → killpg(SIGTERM→50ms→SIGKILL)   (covers a wedged/Noneless runner)                   │
   ▼                                                                                    ▼
child exits ──▶ reaper try_finalize(publish=true) ──▶ park {status:"timed_out", exit_code:124}
   │
   ▼  heartbeat collect_command_completions → supervisor.ingest_completion ("timed_out" → Failed)
      → [BACKGROUND COMPLETED … status=timed_out] → loop drains → model sees it
```

- **Three terminal causes unify:** natural exit, explicit `terminate`, and timeout all just make the child exit; `try_finalize` handles them identically. A timed-out **unpolled** session is finalized by the reaper (`publish=true`) → **heartbeat notifies**; a timed-out **polled** session returns inline via `wait_for_yield` (no notification — exactly-once).
- **OPEN DECISION (no-timeout cap):** add a generous, configurable daemon-side wall-clock cap (`EOS_COMMAND_SESSION_MAX_S`) enforced by the reaper for sessions started **without** an explicit timeout, so orphans can't run forever. Recommended (large default; safety net, not policy). *Pending your call.*

---

## 4. Finalize variants — isolated vs ephemeral (#2)

**Thesis confirmed, with correction:** the biggest difference is **OCC publish + LayerStack lease**, but **isolated finalize tears down *nothing*** — all lease/scratch teardown is deferred to `exit_isolated_workspace`.

| Concern | **  Ephemeral** (`finalize_command_workspace:1296`) | **ISOLATED** (`finalize_isolated_command_workspace:1197`) |
|---|---|---|
| **OCC** | **publishes**: `apply_occ_changeset(root, manifest_version, changes, base_hashes)` (`1323`) | **never publishes** — capture is **record-only**, `published:false` (`1259`); changes stay in private scratch |
| **LayerStack lease** | per-session **acquire** `acquire_snapshot(...)` (`738`) + **release** `release_lease(lease_id)` (`1080-81`) | **untouched** by finalize — lease is **session-level** (acquired at `enter`, released at `exit`); `CommandHandle` has **no `lease_id`** |
| **Teardown** | `remove_dir_all(run_dir)` + `release_lease`, **unconditional, after the finalize `unwrap`** (else the lease leaks on error) | **none** — only `registry.remove(id)` + `unregister_command_session(id)`; scratch/ns torn down at `exit` |
| `changed_paths` | published-only filter (`dispatcher.rs:2755`) | all captured paths (`1230`) |
| `changed_path_kinds` | hardcoded `"write"` | real `layer_change_kind` |
| `conflict` / `mutation_source` | from OCC / `"overlay_capture"` | `null` / `"isolated_workspace"` |
| extra side effect | — | `record_tool_call(agent_id, {…published:false})` (`1275`) |

**Workspace structs** (`command.rs:611-627`):

```rust
struct EphemeralCommandWorkspace {           // Ephemeral — has lease_id + run_dir to release/remove
    root: PathBuf, lease_id: String, manifest: eos_layerstack::Manifest, manifest_version: i64,
    upperdir: PathBuf, run_dir: PathBuf, output_path: PathBuf, final_path: PathBuf,
}
struct IsolatedCommandWorkspace {   // ISOLATED — nothing per-command to release
    handle: crate::isolated::CommandHandle,   // layer_stack_root, scratch_dir, upperdir, …  (no lease_id, no run_dir)
    output_path: PathBuf, final_path: PathBuf,
}
```

**Unified `try_finalize` shape** — prologue and epilogue are byte-identical; a three-point branch on `CommandWorkspaceKind`:

```rust
enum CommandWorkspaceKind { Ephemmeral(EphemeralCommandWorkspace), Isolated(IsolatedCommandWorkspace) }

// 1. Ephemeral prologue (identical): wait child, terminate_pgroup, read runner output,
//    derive(exit_code, status), cancel/interrupt override, completed_session_stdout
// 2. workspace-finalize body (response fields diverge → keep both builders):
let response = match &self.workspace {
    Ephemeral(w)   => finalize_command_workspace(w, …, status),          // OCC publish + guarded_changeset_response
    Isolated(w) => finalize_isolated_command_workspace(w, …, status), // capture-only, published:false
};
// 3. teardown (MUST run even on finalize Err, or shared leaks the lease):
match &self.workspace {
    Ephemeral(w)   => { remove_dir_all(&w.run_dir); LayerStack::open(&w.root).release_lease(&w.lease_id); }
    Isolated(_) => { /* nothing — deferred to exit_isolated_workspace */ }
}
// 4. registry: both registry.remove(id); Isolated also unregister_command_session(id)
// 5. shared epilogue (identical): if publish → push_completed(+notification_result); write final.json
```

**Notification orthogonality:** `publish` (notification) is independent of `CommandWorkspaceKind` — both kinds park on `publish=true`, so an **isolated** background/timed-out session **also** gets a `[BACKGROUND COMPLETED]` notification; its changes just stay in isolated scratch and its lease/scratch cleanup waits for `exit_isolated_workspace`.

---

## 5. agent-core background supervisor (`eos-engine/src/background`)

### 5.1 Before → after

| Aspect | **Before** | **After** |
|---|---|---|
| Command sessions | **none tracked** | `command_sessions: HashMap<String, CommandSessionRecord>` |
| `count` granularity | `background_inflight_count` **ignores `agent_id`** (request-wide) | `count_command_sessions_by_agent(agent_id)` (per-task-run) |
| Daemon pull | none | **heartbeat task** → `collect_command_completions` (the only daemon-pull driver) |
| Completion → model | none (push doesn't exist; `Delivered` never set; sink never drained) | render `[BACKGROUND COMPLETED]` → enqueue sink → loop drains |
| Recover race | n/a | `command_session_result()` + `mark_command_session_reported()` |

### 5.2 New types & methods

```rust
// PROPOSED — eos-engine/src/background/command_session.rs
pub struct CommandSessionRecord {
    pub command_session_id: String,   // daemon-minted "cmd_N" — the correlation key
    pub sandbox_id: String,
    pub agent_id: String,             // = agent_run_id → per-task-run ownership
    pub command: String,
    pub status: BackgroundTaskStatus, // reuse Running→Completed/Failed/Cancelled→Delivered
    pub result: Option<Value>,        // terminal completion payload (None until terminal)
}

impl BackgroundTaskSupervisor {                       // extend existing struct
    pub fn register_command_session(&mut self, id, sandbox_id, agent_id, command);
    pub fn ingest_completion(&mut self, completion: &Value);            // pull → "ok"→Completed/"cancelled"→Cancelled/else→Failed
    pub fn drain_command_session_notifications(&mut self) -> Vec<SystemNotification>; // terminal&&result.is_some → render → Delivered
    pub fn command_session_result(&self, id) -> Option<Value>;          // recover (status != Running)
    pub fn mark_command_session_reported(&mut self, id, result);        // write_stdin → Delivered
    pub fn running_command_session_ids_by_sandbox_agent(&self) -> Vec<((String,String), Vec<String>)>;
    pub fn count_command_sessions_by_agent(&self, agent_id) -> usize;   // terminal-gating
    pub fn cancel_command_sessions_by_agent(&mut self, agent_id);       // request/loop exit → RPC cancel + Cancelled
}
```

### 5.3 Heartbeat poller (the pull driver — D5)

```rust
// PROPOSED — eos-engine/src/background/heartbeat.rs
fn spawn_command_completion_heartbeat(
    supervisor: Arc<Mutex<BackgroundTaskSupervisor>>,
    sink: Arc<dyn NotificationSink>,
    transport: Arc<dyn SandboxTransport>,
) -> JoinHandle<()> {                       // lazy: start on first register; self-stops when no running sessions
    tokio::spawn(async move {
        loop {
            let groups = { supervisor.lock().await.running_command_session_ids_by_sandbox_agent() };
            if groups.is_empty() { break; }                       // idle → terminate
            for ((sandbox_id, agent_id), ids) in groups {
                if let Ok(cs) = collect_command_completions(&*transport, &sandbox_id, &agent_id, &ids).await {
                    let mut sup = supervisor.lock().await;
                    for c in cs { sup.ingest_completion(&c); }                 // refresh records
                    for note in sup.drain_command_session_notifications() {    // emit → enqueue sink
                        let _ = sink.notify_system(note.into()).await;
                    }
                }                                                  // errors swallowed
            }
            sleep(Duration::from_millis(HEARTBEAT_MS /*≈1000*/)).await;
        }
    })
}
```

> Latency note: since the loop no longer pulls, the heartbeat is the sole *passive* completion path → keep `HEARTBEAT_MS ≈ 1000` (vs Python's 60 s backstop). It does **not** affect gating correctness (the terminal-submit gate queries the daemon `command_session_count` directly) or active polling (`write_stdin` hits the daemon directly).

### 5.4 Completion data flow (daemon → model)

```
cmd_3 finishes in the daemon
  └─ try_finalize(publish=true)  (reaper, unpolled)  → registry.completed["cmd_3"] = {result, notification_result}
       │   (the JSON completion map is the ONLY thing crossing the process boundary)
       ▼   heartbeat: collect_command_completions(transport, sandbox_id, agent_id, ["cmd_3"])   [RPC out]
  supervisor.ingest_completion(c)  → record.status = Completed/Failed/Cancelled ; record.result = c.result
       │
       ▼   supervisor.drain_command_session_notifications()
  render  "[BACKGROUND COMPLETED] command_session_id=cmd_3 status=ok exit_code=0
           command: pytest -q
           stdout: <result.output.stdout>"
  record.status → Delivered                          (exactly-once latch)
       │   notify_system(sink, {event:"cmd_3", message})
       ▼
  NotificationSink (queue)
       │   loop-top:  notifier.drain()
       ▼
  loop_.rs:  yield StreamEvent::SystemNotification  +  append_notifications(messages, …)   ← loop is SOLE writer
       │
       ▼   model sees [BACKGROUND COMPLETED] on its NEXT turn
```

---

## 6. Loop + NotificationSink (`eos-engine/src/query`, `notifications.rs`)

### 6.1 Notification model (D4 revised)

- **All** notifications are sink producers: budget rules (75/100/125%), `[BACKGROUND COMPLETED]`, **and** the terminal-submit nudge.
- The loop drains the sink at the **top of every turn** and injects via `append_notifications` (the loop remains the sole writer of `messages`).
- The **only** non-sink control-flow item is the 150% `TerminalNotSubmitted` **failure** (a loop exit, not a notification).

### 6.2 Terminal-submit nudge = a text-return-triggered sink rule

The nudge fires when the most recent assistant turn was a **text return** — i.e. it contains a `ContentBlock::Text` block **and no `ContentBlock::ToolUse`**. Critically, this is **not** merely `tool_uses.is_empty()`, so a **`Reasoning`-only** turn does **not** trigger it.

```rust
// notifications.rs — trigger change
Self::TerminalCallReminder => {
    !ctx.terminal_tools.is_empty()
-       && messages.iter().any(|m| m.role == MessageRole::Assistant)   // fired EVERY turn (noisy)
+       && last_assistant_was_text_return(messages)                     // Text block AND no ToolUse
}

fn last_assistant_was_text_return(messages) -> bool {
    let m = last_assistant_message(messages);
    m.content.iter().any(|b| matches!(b, ContentBlock::Text { .. }))
        && !m.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    // Reasoning-only → no Text block → false → NO nudge
}
```

### 6.3 Block-type handling (`Reasoning` / `Text` / `ToolUse`)

`ContentBlock::Reasoning { text }` (alias `"thinking"`, `message.rs:62-67`) is a distinct "think" block, **dropped on wire re-encode** (`anthropic.rs`/`openai.rs`), streamed as `ReasoningDelta` (separate from `ToolUseDelta`).

| Assistant block | counts as `tool_use`? | `tool_calls_used++`? | triggers terminal nudge? | counts toward 150% ceiling? |
|---|---|---|---|---|
| `ToolUse` (non-terminal) | ✅ (`tool_uses_from_message`, `loop_.rs:43-60`) | ✅ (`ToolUseDelta`, `:139-143`) | no | no |
| `ToolUse` (terminal) | ✅ | ✅ | no → `ToolStop` exit | no |
| `Text` (no tool) | no | no | **YES** | yes (`text_only_no_terminal_turns++`) |
| `Reasoning` (think) | **no** | **no** | **NO** (no `Text` block) | **yes** — kept as no-infinite-loop backstop (see decision) |

- Think is **already** excluded from tool-use and `tool_calls_used` counting.
- **OPEN DECISION (reasoning-only ceiling):** a `Reasoning`-only turn (no `Text`, no `ToolUse`) currently satisfies `tool_uses.is_empty()` and increments the ceiling counter. **Recommended: keep counting it** (bounds infinite-thinking) but **never nudge** it (the nudge requires a `Text` block). Alternative: exclude reasoning-only from the counter (cleaner naming, but a pure-reasoning loop is then unbounded by this counter). *Pending your call.*

### 6.4 Loop-top sequence (diff of `run_query`)

```diff
  loop {
      if terminal_submission_failed(ctx) { yield TerminalNotSubmitted; break; }   // 150% FAILURE (loop exit, non-sink)
-     let notifications = dispatch_rules(messages, ctx);
-     for n in &notifications { yield SystemNotification(n) }
-     append_notifications(messages, &notifications);
+     enqueue_notification_rules(ctx, &ctx.notifier);    // budget 75/100/125% + terminal-return nudge → sink
+     let drained = ctx.notifier.drain().await;          // the ONLY sink consumer
+     for n in &drained { yield SystemNotification(n) }
+     append_notifications(messages, &drained_texts);    // loop remains the sole writer of `messages`
      let run_request = build_query_run_request(ctx, messages).await;
      … stream model turn; count ToolUseDelta; capture final message …
      messages.push(message);
      if tool_uses.is_empty() {
          ctx.text_only_no_terminal_turns += 1;          // bounds any non-tool turn (text or reasoning-only)
          if terminal_submission_failed(ctx) { yield TerminalNotSubmitted; break; }
          continue;                                       // loop-top drain delivers the text-return nudge
      }
      let outcome = dispatch_assistant_tools(ctx, &tool_uses).await?;
      messages.push(tool_result_message(outcome.tool_results));
      if outcome.terminal_result.is_terminal { ctx.exit_reason = ToolStop; break; }   // SUCCESS
  }
```

### 6.5 Loop control-flow & terminal semantics

**Counters:** `tool_call_limit` (budget); `tool_calls_used` (per `ToolUseDelta`); `text_only_no_terminal_turns` (per non-tool turn); **ceiling = `ceil(1.5 × tool_call_limit)`**; **fail gate = `tool_calls_used + text_only_no_terminal_turns ≥ ceiling`**.

```
                          ┌─────────────────────────── run_query: loop { ───────────────────────────┐
                          ▼                                                                          │
              ┌───────────────────────────┐                                                         │
              │ HARD-CEILING CHECK         │  used + text_turns ≥ 1.5×limit ?                        │
              └───────────────────────────┘──── yes ──▶ emit ToolExecutionCompleted{is_error}        │
                          │ no                          exit_reason = TerminalNotSubmitted ── BREAK ─▶ FAIL
                          ▼
        ┌───────────────────────────────────────────────┐
        │ NOTIFICATIONS (loop-top) — SINGLE SINK PATH     │  enqueue_notification_rules():
        │   • budget 75/100/125%                          │     budget + text-return nudge → notify_system
        │   • text-return nudge (last turn = Text & !Tool)│  drained = notifier.drain()   ◀── the ONLY consumer
        │   • [BACKGROUND COMPLETED] (heartbeat-fed)      │  yield + append_notifications(messages, …)
        └───────────────────────────────────────────────┘   ◀── NOTIFICATION RECEIVED HERE (loop is sole writer)
                          │
                          ▼
        ┌───────────────────────────────┐  build_query_run_request → EventSource.stream
        │ stream one model turn          │  ToolUseDelta → tool_calls_used++ ; ReasoningDelta → (ignored)
        └───────────────────────────────┘  AssistantMessageComplete → final message
                          │  push assistant msg → messages
            ┌─────────────┴──────────────┐
            ▼ tool_uses EMPTY?           ▼ tool_uses present
   (Text-only OR Reasoning-only)   ┌────────────────────────────────────┐
   text_only_no_terminal_turns++   │ dispatch_assistant_tools(ctx, uses) │
            │                      │  run tools → tool_results           │
   HARD-CEILING CHECK again?       │  push tool_result_message → messages│
     ≥1.5×limit ─ yes ─▶ FAIL      └────────────────────────────────────┘
     no ─ CONTINUE ◀──────┐                    │ terminal_result.is_terminal?
       (loop-top drain     │         ┌──────────┴───────────┐
        delivers nudge     │         ▼ yes                  ▼ no
        IFF last turn       │   exit_reason = ToolStop   CONTINUE ──┐ (back to top)
        had a Text block)   │   ── BREAK ──▶ SUCCESS                │
                            └──────────────────────────────────────┘
                          └──────────────────────── } // loop ──────────────────────────────────┘
```

Notes on the branches:
- **Text-only turn** (`Text` block, no `ToolUse`) → counts + the loop-top rule nudges next turn.
- **Reasoning-only turn** (`Reasoning` block, no `Text`/`ToolUse`) → counts toward the ceiling (backstop) but **no nudge** (the rule requires a `Text` block).
- **Terminal tool** dispatched → `ToolStop` (success); the terminal result is the agent's outcome.

- **Two exits:** `ToolStop` (a tool whose result `is_terminal` was dispatched — success) and `TerminalNotSubmitted` (`tool_calls_used + text_only_no_terminal_turns ≥ ceil(1.5 × tool_call_limit)` — failure).
- **Text-only is NOT terminal** (intentional inversion of the Anthropic/OpenAI "no-tool = done" convention): the terminal *tool* is the persisted output channel, so a bare text answer is treated as "not finished." The loop re-prompts — nudged by the text-return rule + budget warnings — until a terminal tool lands (`ToolStop`) or the 150% ceiling fails it (`TerminalNotSubmitted`).
- **Known false-negative:** "completed work but never called a terminal tool" looks identical to "failed" — the text-return nudge + system prompt are load-bearing mitigations; the 150% ceiling caps wasted turns.

### 6.6 Sink changes (`notifications.rs`)

```diff
  pub struct NotificationService { queue: Arc<Mutex<VecDeque<ToolNotification>>> }
  impl NotificationService { pub async fn drain(&self) -> Vec<ToolNotification> {…} }   // now called by the loop
- pub fn dispatch_rules(messages, ctx) -> Vec<SystemNotification>     // returned, injected directly
+ pub fn enqueue_notification_rules(ctx, sink)                        // budget + text-return nudge → notify_system
  // NotificationRule enum kept; TerminalCallReminder trigger → last_assistant_was_text_return; NO cancel(event)
```

---

## 7. Wiring (`eos-runtime/src/entry.rs`) — instance-identity invariant

```diff
  let supervisor = Arc::new(SharedSubagentSupervisor::default());                 // one per request
+ let notifier   = NotificationService::new();                                   // one per request
  let supervisor_port: Arc<dyn SubagentSupervisorPort>        = supervisor.clone();
+ let cmd_port:   Arc<dyn CommandSessionSupervisorPort>       = supervisor.clone(); // SAME instance
+ let sink:       Arc<dyn NotificationSink>                   = Arc::new(notifier.clone());
+ let _heartbeat = spawn_command_completion_heartbeat(supervisor.inner(), sink.clone(), state.transport.clone());
  // RootAgentParams / agent_runner: + command_session_supervisor: cmd_port, + notifications: sink,
  //                                 + notifier (concrete → QueryContext for the loop drain)
```

> **Invariant:** the port handed to tools and the concrete `notifier`/supervisor handed to the loop must wrap the **same** `NotificationService` and the **same** `Arc<Mutex<BackgroundTaskSupervisor>>` — else it compiles and silently delivers nothing.

---

## 8. Exactly-once delivery (D8)

```
command exits
   ├─ a tool is polling (write_stdin/exec_command in wait_for_yield):
   │     try_finalize(publish=false) → terminal returned INLINE → NOT parked → heartbeat never sees it   ✅ one (tool return)
   └─ no tool polling (fire-and-forget / timeout):
         reaper try_finalize(publish=true) → parks → heartbeat → [BACKGROUND COMPLETED]                  ✅ one (notification)

residual race (reaper parks, then a late write_stdin):
   write_stdin reads the cached result via the `finalized` latch and (record already Delivered)
   returns a terse "session already completed (reported)" — single latch check, one place. No cancel(event).
```

---

## 9. Resulting file/folder structure

```
sandbox/crates/eos-daemon/src/
  command/                         # (split command.rs for sense 2)
    mod.rs                         # op_* dispatch (exec_command, write_stdin, cancel, collect_completed, count)
    session.rs                     # CommandSession {+child,+workspace,+finalized,+timeout_deadline}, try_finalize, wait_for_yield, output ring/cursor
    registry.rs                    # CommandSessionRegistry { sessions, completed }, push/take/collect
    reaper.rs                      # NEW: command_session_reaper_sweep, recover_orphaned_command_sessions
    workspace.rs                   # finalize_command_workspace / finalize_isolated_command_workspace, CommandWorkspaceKind
  server.rs                        # serve(): + spawn reaper, + startup orphan recovery
  (eos-runner: timeout enforcement unchanged — primary; eos-terminal-pair unchanged)

agent-core/crates/eos-engine/src/
  background/
    supervisor.rs                  # BackgroundTaskSupervisor (+command_sessions map & methods)
    command_session.rs             # NEW: CommandSessionRecord + ingest/render/recover/mark
    heartbeat.rs                   # NEW: spawn_command_completion_heartbeat (the pull driver)
  query/
    loop_.rs                       # drain sink each turn; sole messages writer; daemon-free
    context.rs                     # QueryContext: + notifier handle
  notifications.rs                 # drain wired; enqueue_notification_rules; text-return nudge trigger

agent-core/crates/eos-tools/src/
  ports.rs                         # NEW: CommandSessionSupervisorPort (register/result/mark/count)
  metadata.rs                      # ExecutionMetadata: + command_session_supervisor (notifications already present)
  model_tools/sandbox.rs           # write_stdin: +terminate, −\x03 escalation; expanded descriptions; stderr documented always-empty
  model_tools/snapshots/…default_tool_specs.snap   # re-pinned (descriptions + terminate field)

agent-core/crates/eos-runtime/src/
  entry.rs                         # one NotificationService + one supervisor; instance identity; spawn heartbeat
  root_agent.rs / agent_runner.rs  # thread the new port + sink + notifier
```

---

## 10. Class & field name reference

**Daemon (new/changed)**

| Name | Kind | Key fields / signature |
|---|---|---|
| `CommandSession` | struct (changed) | `+ child: Mutex<Option<Child>>`, `+ workspace: CommandWorkspaceKind`, `+ finalized: Mutex<Option<Value>>`, `+ timeout_deadline: Option<Instant>` |
| `CommandWorkspaceKind` | enum (new) | `Ephemeral(EphemeralCommandWorkspace)` \| `Isolated(IsolatedCommandWorkspace)` |
| `CommandSession::try_finalize` | fn (new) | `(&self, publish: bool) -> Option<Value>` (idempotent latch) |
| `wait_for_yield` | fn (new) | `(&Arc<CommandSession>, u64) -> WaitOutcome` |
| `WaitOutcome` | enum (new) | `Completed(Value)` \| `Running(String)` |
| `command_session_reaper_sweep` | fn (new) | timeout-kill + `try_finalize(true)` over live sessions |
| `recover_orphaned_command_sessions` | fn (new) | startup scan of `runtime/command-sessions/*` |
| `CommandSessionFinalizer` / `IsolatedCommandSessionFinalizer` | struct (**deleted**) | replaced by `try_finalize` + reaper |

**agent-core (new/changed)**

| Name | Kind | Key fields / signature |
|---|---|---|
| `CommandSessionRecord` | struct (new) | `command_session_id, sandbox_id, agent_id, command, status, result` |
| `BackgroundTaskSupervisor` | struct (changed) | `+ command_sessions: HashMap<String, CommandSessionRecord>` |
| `CommandSessionSupervisorPort` | trait (new, sealed) | `register / command_session_result / mark_command_session_reported / count_by_agent` |
| `spawn_command_completion_heartbeat` | fn (new) | per-request pull task |
| `ExecutionMetadata` | struct (changed) | `+ command_session_supervisor: Option<Arc<dyn CommandSessionSupervisorPort>>` |
| `QueryContext` | struct (changed) | `+ notifier: NotificationService` (concrete drain handle) |
| `WriteStdinInput` | struct (changed) | `+ terminate: bool` |

---

## 11. Sequencing (slices) & verification

Each slice lands **green** (compiles + its tests pass) before the next.

1. **Slice 1 — Supervision + notification (agent-core).** `CommandSessionRecord`, supervisor methods, `CommandSessionSupervisorPort`, heartbeat, sink-drain in loop, rules → sink, text-return nudge trigger, `entry.rs` wiring.
   - **Tests:** register → pull → `Completed` flips `count_command_sessions_by_agent`; recover race (daemon `command_session_not_found` → supervisor returns stored terminal); notify-exactly-once (`Delivered` suppresses the second); **end-to-end: a backgrounded completion lands as a `SystemNotification` in the next provider request**; `messages` sole-writer (no external `&mut`); reasoning-only turn does **not** nudge; text-only turn **does** nudge.
2. **Slice 2 — Daemon sense 2.** `Child`-in-session, `try_finalize`, `wait_for_yield`, reaper (+ timeout backstop), orphan recovery, UTF-8 carry-over, `CommandWorkspaceKind` branch (ephemeral + isolated).
   - **Tests:** inline finalize == parked finalize result; quiet-after-output early return; `write_stdin` returns before full yield on completion; timeout → `timed_out` parked → collectable; reaper finalizes unpolled; lease released even on finalize `Err`; isolated finalize publishes nothing and releases no lease; daemon-restart orphan reaping.
3. **Slice 3 — Tool polish.** `terminate` flag, drop `\x03` escalation, expand `exec_command`/`write_stdin` descriptions, document `stderr` always-empty, re-pin `default_tool_specs.snap`.

---

## 12. Open decisions / risks

- **No-timeout default cap** (§3) — add `EOS_COMMAND_SESSION_MAX_S` reaper cap for sessions without an explicit timeout? *Recommended yes (large default).*
- **Reasoning-only ceiling** (§6.3) — keep counting reasoning-only turns toward the 150% ceiling (backstop) vs exclude. *Recommended keep.*
- **Retire the every-turn generic terminal reminder** in favor of the text-return-triggered one? *Recommended yes (less noise; Python parity divergence is acceptable).*
- **`QUIET_MS` ≈ 50**, **`HEARTBEAT_MS` ≈ 1000** — env-tunable defaults.
- **`stderr`** stays in the result shape, documented "always empty (PTY merges stderr into stdout)" — not removed (keeps the JSON contract stable).
- **Risk:** sense-2 rewrites the daemon's core session lifecycle (finalize/timeout/cancel + isolated twin) on a passing daemon — Slice 2 must preserve the runner-side timeout (primary) and the lease-release-on-error guarantee.
