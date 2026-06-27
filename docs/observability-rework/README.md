# Observability Rework — Spec

Status: ready-to-implement.

Collapse all observability into **one dependency-light crate**
(`sandbox-observability`) backed by an **append-only NDJSON event stream**,
replacing both the SQLite snapshot store and the scattered `timing.rs` modules.
One model — `span` / `event` / `sample` — owns spans, traces, events, and
resource metrics; the runtime emits into it directly; a developer fetches it with
one command.

Two facts shape the whole design:

- **A span is one record, written when it completes.** It carries `ts`
  (completion) + `dur_ms`, so the full waterfall is reconstructable from a single
  line. There is no start/end pair to reconcile.
- **Current state is not read from the log.** "What workspaces exist / what's in
  flight right now" already lives in the runtime's in-memory
  `observability_snapshot()`. The snapshot view reads *that*; the NDJSON log is
  the **historical** record (completed spans, events, samples). This is why we can
  drop SQLite without re-adding a queryable store.

This spec is **example-driven**: §4 shows the literal bytes that land in the log
and the rendered views, for real scenarios. The rest is structure around those
examples.

> **Companion:** `layerstack-impl.md` extends this spec with a third
> `collect/` reader (layer count / per-layer bytes from disk), leased/booked-by
> counts from the runtime registry, and a cgroup `io.stat` field — additive, no
> record-kind or schema change.

---

## 1. What replaces what

| Today | After |
|---|---|
| `sandbox-observability` = SQLite store of 4 snapshot tables, `rusqlite` | same crate, an append-only NDJSON stream, `serde_json` only |
| `sandbox-daemon/src/observability` = a *second* "observability" (collect + RPC + delta caches + reconcile) | thin caller; cgroup/disk readers fold into the crate; entity state + in-flight served straight from the live runtime snapshot |
| 5 in-sandbox `timing.rs`, ~56 in-sandbox `timing::duration` sites, `EOS_*_TIMING` forwarded into the container | one `Observer` API; the dotted labels become span `name`s |
| no cross-process correlation, no fetch path | `trace` ids (= `Request.request_id`) + one `get_observability` RPC + `sandbox-cli … observability` |

Why a rewrite and not a patch: "one layer" + "drop SQLite" *forces* it — SQLite
is precisely what keeps this crate out of the runtime today (there is a test
whose only job is to keep it out: `operation/tests/observability_snapshot.rs:91`
asserts the runtime manifest contains neither `sandbox-observability` nor
`rusqlite`). Remove SQLite and the crate becomes a leaf the runtime can emit
into. The win is **dependency weight and one layer** — `rusqlite` + 8 migrations +
the second daemon observability layer go away. It is *not* a net-LOC reduction:
the new internals add a `Reader` + view folds, so expect comparable line count,
fewer dependencies, and one home instead of two.

---

## 2. Architecture

Spans/events/samples are produced **in the sandbox**, written to **one file**.
Current state (sandbox/workspace/in-flight) is read live from the runtime. Both
are pulled to the **host** over the daemon protocol.

```
                 IN SANDBOX (container)                                    HOST
 ┌──────────────────────────────────────────────────────────┐   ┌──────────────────────┐
 │ sandbox-daemon  (one process)                             │   │ sandbox-gateway CLI  │
 │                                                            │   │  (host-side timing   │
 │  runtime libs: operation / workspace / layerstack /        │   │   retained, separate)│
 │                namespace-execution                         │   │                      │
 │        │ obs.span(…)  obs.event(…)  obs.sample(…)           │   │                      │
 │        ▼                                                   │   │                      │
 │     Observer ──────► Sink (file, O_APPEND, one write/line) │   │                      │
 │                          │                                 │   │                      │
 │                          ▼                                 │   │                      │
 │   <runtime_dir>/observability/observability.ndjson         │   │                      │
 │                          ▲              ▲                  │   │                      │
 │  namespace-process ──────┘ (forked,     │ Reader (history) │   │                      │
 │     obs.span(…)            same file)    │                 │   │                      │
 │                                          │                 │   │                      │
 │  runtime registry ── observability_snapshot() ─┐ (live)    │   │                      │
 │                                                ▼           │   │                      │
 │  DaemonObservability ── get_observability RPC ◄────────────┼───┤ sandbox-cli <id>     │
 │                         └──── view JSON ───────────────────┼──►│   observability …    │
 └──────────────────────────────────────────────────────────┘   └──────────────────────┘
```

- **Write:** every in-sandbox process appends single-line records, each in one
  `write()` syscall.
- **Store (history):** one NDJSON file at
  `<daemon_runtime_dir>/observability/observability.ndjson` (the path `paths.rs`
  derives today, retargeted from `.sqlite`).
- **Current state:** the runtime's in-memory `observability_snapshot()` (sandbox
  state, workspaces, in-flight executions) — not the log, not a store.
- **Read:** a `Reader` folds the log into the trace / samples / raw views; the
  daemon serves the snapshot view from the live snapshot; the CLI renders both.
- **Host-side gateway/CLI timing is out of scope** and retained — the in-sandbox
  `Sink` isn't reachable from the host process (§8.1).

---

## 3. The record model

One JSON object per line. Three `kind`s. Shared envelope: `ts` (unix ms,
occurrence/completion time of *this record*), `kind`, `trace`. The file is one per
sandbox, so `sandbox` is **not** stamped per line; there is **no** `component`/`pid`
(no view renders them, and the `<proc>` token + span `name` already identify origin).
Append order ≈ `ts` order; the reader sorts by `ts` and resolves `span`/`parent` by
id, so it never depends on append order.

### 3.1 `span` — a completed unit of work, **one** record

```json
{"ts":1719500004273,"kind":"span","trace":"req-7f3","span":"d-5","parent":"d-1","name":"namespace.exec.shell","dur_ms":4231.0,"status":"completed","attrs":{"exec_id":"ns-9","async":true,"exit_code":0}}
```

`ts` = completion time; `dur_ms` = wall duration (so **start = `ts - dur_ms`**);
`trace` groups one flow; `span` = a **process-unique** id (`<proc>-<seq>`, see
below); `parent` builds the tree; `status`/`attrs` (incl. `exit_code`) describe the
outcome.

**Span id uniqueness.** Multiple processes append to one file under one `trace`
(Case A: the daemon process and the forked namespace-process). Per-process
counters would collide, so a span id is `<proc>-<seq>` where `<proc>` is a token
assigned once per process (pid- or random-derived). Examples below use `d-*` for
the daemon/runtime process and `np-*` for the namespace-process.

**Why one record (not start+end).** The async work outlives the call (Case A: the
shell exec returns at ~1.05 s, the span closes at ~4.27 s on the watcher thread).
We still write **one** record — at completion, on whichever thread finishes the
work — and `ts`/`dur_ms` reconstruct the bar. This halves write volume on hot
paths, removes the start↔end pairing reducer, and removes the
permanent-unpaired-start-after-crash failure mode.

**What's in flight right now is NOT in the log.** A span has no record until it
completes, so the log can't answer "what's running." It doesn't need to: the
runtime already holds in-flight executions in memory
(`observability_snapshot().active_namespace_executions`), and the snapshot view
reads that directly (Case B). The log is purely historical.

### 3.2 `event` — a point-in-time domain fact, hung off a span

```json
{"ts":1719500004295,"kind":"event","trace":"req-7f3","parent":"d-6","name":"lease.released","attrs":{"revision":"r5"}}
```

Side-effects (lease, state transition, error) — carry `trace`/`parent`
so they attach to the originating flow even when emitted in the async tail. An
event's record can appear *before* its parent span's record (the parent completes
last); the reader resolves `parent` by id.

### 3.3 `sample` — a periodic metric reading (cgroup + disk)

```json
{"ts":1719500000000,"kind":"sample","scope":"ws-1","cpu_usec":12345,"mem_cur":1048576,"mem_max":2097152,"disk_bytes":40960,"files":12,"dirs":3,"symlinks":0,"truncated":false}
```

`scope` = `"sandbox"` or a workspace id. Cumulative counters are stored raw;
**deltas are not stored** — the reader computes them from adjacent samples (§4.4).

---

## 4. Example cases — the observability outputs

Each case shows the **trace diagram**, the **raw NDJSON** (append order = `ts`
order), and the **rendered view** a developer sees. Span `ts` is completion time;
the renderer plots each bar from `ts - dur_ms`.

### 4.1 Case A — one-shot `exec_command` (sync call + async tail + finalize-teardown)

A client runs a command with no existing session. The runtime creates a one-shot
workspace (mount over a layerstack lease), runs the shell, and on child exit
finalizes the workspace **on the watcher thread, after the call already
returned**: capture upperdir changes, publish them to the layerstack, refresh the
session handle, then destroy the workspace and release the original lease.

```
req-7f3   command.exec  (one-shot)
 ├─ daemon.dispatch ─────────────────────────── returns at yield (~1.05s)
 │   └─ command.exec
 │       ├─ workspace_session.create
 │       │   • lease.acquired r5
 │       │   └─ namespace.exec.mount_overlay   (sync mount guard)
 │       └─ namespace.exec.shell               [async] ── outlives the call ──┐
 │           └─ namespace.runner.spawn_child  (namespace-process · Phase B)   │
 └─ ── watcher thread, after return ──                                        │
     ├─ workspace_session.capture_changes     ◄──────────────────── child exits
     ├─ layerstack.publish r5→r6
     └─ workspace_session.destroy (one-shot)
         • lease.released r5
```

**Raw `observability.ndjson` (append order = `ts` order; spans land at
completion):**

```json
{"ts":1719500000009,"kind":"event","trace":"req-7f3","parent":"d-2","name":"lease.acquired","attrs":{"revision":"r5"}}
{"ts":1719500000040,"kind":"span","trace":"req-7f3","span":"d-4","parent":"d-2","name":"namespace.exec.mount_overlay","dur_ms":27.0,"status":"completed"}
{"ts":1719500000042,"kind":"span","trace":"req-7f3","span":"d-2","parent":"d-1","name":"workspace_session.create","dur_ms":39.0,"status":"completed"}
{"ts":1719500000061,"kind":"span","trace":"req-7f3","span":"np-0","parent":"d-5","name":"namespace.runner.spawn_child","dur_ms":6.0,"status":"completed","attrs":{"exec_id":"ns-9"}}
{"ts":1719500001050,"kind":"span","trace":"req-7f3","span":"d-1","parent":"d-0","name":"command.exec","dur_ms":1048.0,"status":"completed","attrs":{"one_shot":true}}
{"ts":1719500001051,"kind":"span","trace":"req-7f3","span":"d-0","name":"daemon.dispatch","dur_ms":1051.0,"status":"completed","attrs":{"op":"exec_command"}}
{"ts":1719500004273,"kind":"span","trace":"req-7f3","span":"d-5","parent":"d-1","name":"namespace.exec.shell","dur_ms":4231.0,"status":"completed","attrs":{"exec_id":"ns-9","async":true,"exit_code":0}}
{"ts":1719500004286,"kind":"span","trace":"req-7f3","span":"d-6","parent":"d-1","name":"workspace_session.capture_changes","dur_ms":11.0,"status":"completed","attrs":{"one_shot":true}}
{"ts":1719500004299,"kind":"span","trace":"req-7f3","span":"d-7","parent":"d-1","name":"layerstack.publish","dur_ms":12.0,"status":"completed","attrs":{"base":"r5","revision":"r6","layers_added":1,"bytes":40960,"no_op":false}}
{"ts":1719500004320,"kind":"event","trace":"req-7f3","parent":"d-8","name":"lease.released","attrs":{"revision":"r5"}}
{"ts":1719500004325,"kind":"span","trace":"req-7f3","span":"d-8","parent":"d-1","name":"workspace_session.destroy","dur_ms":25.0,"status":"completed","attrs":{"one_shot":true}}
```

Note `d-1`/`d-0` complete at ~1.05 s while `d-5` is still running (no record yet).
`d-5` carries the child-exit instant as its `ts` (4.273 s, captured **before** the
finalize tail); the watcher then writes `capture_changes` (`d-6`), `layerstack.publish`
(`d-7`), and `workspace_session.destroy` (`d-8`) under `d-1` — the originating
`command.exec`, not the shell span. The mount (`d-4`) is a **sync** span under
`workspace_session.create`; the `d-3` slot is vacant — `workspace.create` was
dropped (C1) — so the shell stays `d-5` and Phase B's `np-0.parent = d-5`
resolves. `layerstack.publish` carries the new revision (`r6`); `lease.released`
still reports the released original lease revision (`r5`). The reader orders by
`ts`/`parent`, never by append order — all under `req-7f3`.

**Rendered — `sandbox-cli observability trace --sandbox-id eos-abc --id req-7f3`:**

```
trace req-7f3   sandbox eos-abc   wall 4.33s   (call returned at 1.05s)

  +00.000  daemon.dispatch op=exec_command                 1051ms  ✓
  +00.002   └ command.exec one_shot                        1048ms  ✓
  +00.003      ├ workspace_session.create                    39ms  ✓
  +00.009      │   • lease.acquired r5
  +00.013      │   └ namespace.exec.mount_overlay            27ms  ✓
  +00.042      ├ namespace.exec.shell           [async]    4231ms  ✓ exit0   ← outlives call
  +00.055      │   └ namespace.runner.spawn_child            6ms  ✓   [Phase B: cross-process]
  +04.275      ├ workspace_session.capture_changes           11ms  ✓
  +04.287      ├ layerstack.publish r5→r6 +1 layer 40KB      12ms  ✓
  +04.300      └ workspace_session.destroy one_shot          25ms  ✓
  +04.320         • lease.released r5
```

**Phase split for this case.** Everything renders under `req-7f3` in **Phase A**
*except* `np-0` — it is emitted by the forked namespace-process, and its
`trace`/`parent` only cross the fork in **Phase B**. Until then `np-0` appears
under its own trace. The async tail (`d-5`, `d-6`, and the `lease.released`
event) is in-process (the watcher runs in the daemon process), so it correlates
in Phase A — this is the threading work §9.3 calls out.

### 4.2 Case B — persistent session + an in-flight snapshot

A long-running command against an existing session. While it runs, the developer
asks for the live snapshot. The command shows up as **in flight** — sourced from
the runtime registry, **not** from the log (the span has no record until it
completes).

**Raw log so far** (only the completed span is present):

```json
{"ts":1719500101020,"kind":"span","trace":"req-9a1","span":"d-10","name":"command.exec","dur_ms":1020.0,"status":"completed","attrs":{"workspace_session":"ws-7","one_shot":false}}
```

The async `namespace.exec.shell` (`ns-42`) is still running, so it has **no line
yet**. The in-flight row comes from `observability_snapshot()`.

**Rendered — `sandbox-cli observability snapshot --sandbox-id eos-abc`:**

```
sandbox eos-abc   state ready        (state · workspaces · in-flight from live runtime registry)

  workspaces
    ws-7   active    profile=default   layers=4

  in-flight executions            (from runtime registry, not the log)
    ns-42  namespace.exec.shell   trace req-9a1   running 7.3s   ws-7

  resources (latest)             (latest sample per scope, from the log)
    sandbox   cpu 12.3s   mem 41MB / 256MB
    ws-7      cpu  4.1s   mem 18MB        disk 1.2MB (320 files)
```

### 4.3 Case C — resource samples + deltas

Periodic `sample` lines; the reader computes deltas at read time (none stored).

**Raw:**

```json
{"ts":1719500000000,"kind":"sample","scope":"ws-1","cpu_usec":1000000,"mem_cur":18000000,"disk_bytes":1200000,"files":320}
{"ts":1719500010000,"kind":"sample","scope":"ws-1","cpu_usec":4100000,"mem_cur":21000000,"disk_bytes":1320000,"files":340}
{"ts":1719500020000,"kind":"sample","scope":"ws-1","cpu_usec":4250000,"mem_cur":20500000,"disk_bytes":1320000,"files":340}
```

**Rendered — `sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window 60000`:**

```
scope ws-1   window 60s   (Δ computed at read)

  t(+s)   cpu_total   Δcpu      mem_cur    disk        Δdisk
  00.0     1.00s        –       18.0MB     1.20MB        –
  10.0     4.10s     +3.10s     21.0MB     1.32MB     +120KB
  20.0     4.25s     +0.15s     20.5MB     1.32MB        +0
```

---

## 5. Emit seams — where the `Observer` plugs in

Hook the **existing** lifecycle edges; do not sprinkle inline timing.

```
 sync scope        →  obs.scope("workspace_session.create", |s| { … })  (RAII guard; one record on drop; .attr()/.status(); Error on Err)
 sync ns mount     →  let _s = obs.span("namespace.exec.mount_overlay");  (mount_overlay is .wait()ed → sync guard)
 async ns exec     →  caller: SpanRegistry::launch (ctx, "namespace.exec.<kind>") at launch
                      TerminalHook::on_terminal  → write the one span record at child-exit (watcher thread)
 layerstack facts  →  acquire_snapshot_with_lease → event lease.acquired / lease.released
                      publish_changes            → span layerstack.publish (status=error + reason on conflict)
 cgroup/disk       →  daemon collect() → obs.sample(scope, metrics)   (readers moved into the crate)
```

- **trace id = `Request.request_id`** (`sandbox-protocol/src/request.rs:11`),
  required on every daemon request. Set a thread-local `TraceContext` at
  `dispatch_request` (`sandbox-daemon/src/server/dispatch.rs:54`);
  `dispatch_operation(&operations, &request)` runs the operation synchronously on
  that thread, so in-daemon sync spans form the per-request tree via the
  thread-local parent. **Do not** use the layerstack `owner_request_id`
  (`layerstack/src/stack/mod.rs:75`) — it is validated and immediately discarded
  (not stored in the lease record, never returned, only ever the literal
  `"workspace-session"` in production), so it can correlate nothing.
- The engine-facing hook `ExecutionObserver` (`namespace-execution/src/types.rs:19`)
  is wired as `NoopObserver` (`operation/src/command/service/core.rs:34`). Replace it
  with the generic `TerminalHook<NamespaceExecutionId>` hook (`crate-core-impl.md`
  §3.4); the recording impl **is** the generic `SpanRegistry<NamespaceExecutionId>`
  itself, via a blanket `impl<K> TerminalHook<K> for SpanRegistry<K>` (§6) — no
  bespoke `NamespaceExecutionObserver` adapter. Because the engine hands the
  terminal edge **only** the exec id, the **caller** `SpanRegistry::launch`es
  `(ctx, "namespace.exec.<kind>")` at launch (`engine.rs` shell, where kind + ctx are
  in scope), parking an open span; `on_terminal` (watcher thread, called right after
  child-exit and **before** teardown) pops it and writes the single span record via
  `record`. There is no `on_running` and no timestamp — one record, recorded at the
  completion call. (The overlay mount is `.wait()`ed, so it is a **sync** `SpanGuard`,
  not a parked async span — C1.)
- The one-shot finalize (`exec_command.rs:181`) runs inside that watcher but
  **captures no context today** — snapshot the request's `TraceContext` on the
  dispatch thread (at closure construction, not inside the `move`) so its `destroy`
  span and `lease.released` event land under the originating trace (Case A tail).
  Teardown must run whether or not context was captured — observability must not
  change behavior (M4). This in-process threading is Phase-A work (§9.3), distinct
  from the cross-process threading deferred to Phase B.

---

## 6. Crate rework — keep / reshape / delete

| File | Fate | Note |
|---|---|---|
| `src/paths.rs` | **Keep** | `database_path()` → `log_path()` (`observability.ndjson`) |
| `src/records.rs` | **Reshape** | drop per-field bounds/validators; structs → `Span`/`Event`/`Sample` (one `MAX_LINE_BYTES` cap) |
| `src/store.rs` | **Delete+replace** | `rusqlite` store → `Sink` (append `O_APPEND` writer, one write/line) |
| `src/store/schema.rs` | **Delete** | 8 migrations gone |
| `src/store/{read,rows}.rs` | **Delete+replace** | SQL queries → `Reader` + history views |
| `src/lib.rs` | **Rewrite** | export `Observer`, `SpanGuard`, `SpanRegistry`, `TerminalHook`/`NoopHook`, `TraceContext`, `Reader`, record + view types |
| `Cargo.toml` | **Edit** | drop `rusqlite`; add `serde`/`serde_json` |

```
crates/sandbox-observability/src/
  lib.rs            paths.rs            record.rs        emit.rs        read.rs
  collect/{mod.rs,  cgroup.rs,  disk.rs}      ← moved from sandbox-daemon (pure &Path → struct)
```

**Atomicity / bounds.** Each record is serialized — **including its trailing
`\n`** — into one buffer and emitted with a **single `write()`** to an `O_APPEND`
fd. On a local Linux fs (incl. the in-container tmpfs/overlay) the kernel
serializes concurrent `write()`s per inode, so one-syscall line appends from the
daemon and the forked namespace-process never interleave — **at any line length**.
(`PIPE_BUF` is a pipe/FIFO guarantee and does not apply to regular files; what
matters is one syscall per line, not a 4096 B ceiling.) A **total
serialized-line cap** is enforced at emit: if a line would exceed it, `attrs` are
truncated to fit — the line is never split. The reused `MAX_*` field bounds are
necessary but not sufficient for this (one 4096 B path attr already nears the
budget), so the cap is on the whole serialized line. A soft size cap with one
rotation (`…ndjson.1`), **owned by the daemon**, bounds the file; the reader reads
both, ordered by `ts`. Rotation drops the oldest history; no marker record is
written, so an empty trace renders as "unknown trace, or rotated out."

**Redaction.** `attrs` and event payloads MUST NOT carry raw command lines, env,
or secret-bearing paths — the file is shipped to the host over the daemon RPC.
`name`s are `&'static str`; attrs are bounded and truncated at the line cap.

**Emit API (shape):**

```rust
impl Observer {                                                // Clone; one per process
    fn span(&self, name: &'static str) -> SpanGuard;            // sync; thread-local parent; one record on drop
    fn scope<T, E>(&self, name: &'static str,                   // fallible sync scope: Error on Err, then drop
                   body: impl FnOnce(&SpanGuard) -> Result<T, E>) -> Result<T, E>;
    fn event(&self, name: &'static str, attrs: impl Into<Value>);          // thread-local parent
    fn sample(&self, scope: &str, metrics: impl Into<Value>);
    fn context(&self) -> Option<TraceContext>;
    fn with_context<R>(&self, ctx: impl Into<Option<TraceContext>>, f: impl FnOnce() -> R) -> R;
}

impl SpanGuard {                         // sync, !Send: ends on drop, same thread
    fn attr(&self, key: &'static str, value: impl Into<Value>) -> &Self;   // accumulate facts (chainable)
    fn status(&self, status: SpanStatus) -> &Self;                         // override default Completed (chainable)
}

// generic park-by-key store + the generic engine hook (crate-core-impl.md §3.4)
impl<K: Eq + Hash> SpanRegistry<K> {
    fn open(&self, id: K, ctx: TraceContext,                               // park + self-stamp start;
            name: &'static str) -> TraceContext;                           //   returns child ctx (parent = new span id)
    fn launch<T, E>(&self, id: K, ctx: impl Into<Option<TraceContext>>,    // open → run f → cancel on Err
                    name: &'static str, f: impl FnOnce(Option<TraceContext>) -> Result<T, E>) -> Result<T, E>;
    fn record(&self, id: &K, status: SpanStatus, attrs: impl Into<Value>); // pop + write one record (self-stamps end)
    fn cancel(&self, id: &K);                                              // pop without writing
}
trait TerminalHook<K> { fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>); }
impl<K: Eq + Hash> TerminalHook<K> for SpanRegistry<K> { /* folds exit_code + async:true, then record */ }
```

`TraceContext = { trace, parent }` — the two ids needed to cross a thread or
process boundary; the sync `span()` reads the thread-local context set at dispatch,
and `event` attaches by `parent` likewise. **Consolidations vs. a naïve
port:** one record per span (no start/end split); no separate `Scope` type (`sample`
takes `scope: &str`); `Attrs` is a `serde_json::Map` alias, not a new type; the emit
API never takes a timestamp (the `Observer` self-stamps). `SpanGuard` (sync, ends on
drop, same thread) and the parked async span in `SpanRegistry<K>` (completed by an
id-only callback on another thread) are a *real* split — drop-on-same-thread vs.
complete-by-key-from-another — but each writes exactly one record. There is **no**
standalone async-span handle: the only async shape is the parked one, so the registry
owns the open span as plain data and `record` writes it (self-stamping the end). A
new async source is a new `K` + its own `SpanRegistry<K>` (wired as the engine's
`TerminalHook<K>` by the blanket impl), not new map/lock code. The engine-facing hook `TerminalHook<K>`
carries only the terminal edge (`on_terminal`) — one-record-at-completion needs no
`on_running`, and no timestamp (the span is recorded at the completion call).

Emit is config-gated (`observability.enabled`, default on in-sandbox; off for the
host CLI unless a flag) and near-free when disabled. Observability MUST never
fail the operation it observes — over-long attrs truncate, errors are swallowed.

**Reader (shape), over the log:**

- `trace(id)` — the waterfall: filter by `trace`, build the tree by `parent`,
  order siblings by start (`ts - dur_ms`), offset each node by
  `(ts - dur_ms) - trace_start`. Events render at their `ts` under `parent`.
- `samples(scope, window)` — filter by `scope` and `ts ≥ now - window`, sort by
  `ts`, compute pairwise deltas between adjacent samples per scope. Backs both
  `cgroup` (`scope` = `sandbox`/`<ws>`) and `layerstack` (`scope` = `stack`).
- `raw(filter)` — single forward scan, filter-while-reading (`kind`, `name`,
  `since_ms`, `trace`). The `events` view is `raw{ kind:"event", name }` parsed by
  the CLI — no dedicated method.

The **`snapshot()` view does not read the log** — it returns the runtime's live
`observability_snapshot()` (sandbox state, workspaces, in-flight executions),
joined with the latest `sample` per scope. For a *currently-running* trace,
`trace(id)` may merge the live in-flight spans on top of the completed records.

**Boundary.** `sandbox-observability` stays a **leaf** (`serde`, `serde_json`,
`thiserror` only — never `protocol`/`runtime`/`daemon`/`config`). All dependency
edges point into it; the graph stays acyclic. That leaf-ness is what lets the
runtime emit into it.

---

## 7. Fetch — one op, explicit subcommands

The current single op `get_observability_snapshot`
(`sandbox-daemon/src/server/dispatch.rs:9`, handler `:87`) is **generalized** into
one op whose `view` picks **what you're checking against** — so the command always
names its target (trace vs event vs cgroup vs layerstack), never an overloaded
flag:

```
op: "get_observability"
params: { view:"snapshot"|"trace"|"events"|"cgroup"|"layerstack"|"raw",
          trace?, name?, scope?, workspace?, samples?,
          since_ms?, window_ms? (≤600_000), kind? }
→ JSON of the view
```

Every command is **`sandbox-cli observability <view> --sandbox-id <id> [flags]`**.
`--sandbox-id` selects the target daemon (no longer a positional). `snapshot` is
served from the live runtime snapshot; the rest are folded from the log (a forward
scan of up to two files, bounded by the size cap). **Pull is one-shot** —
`--follow` is out of scope; re-poll for a running command.

| `<view>` | Checks against | Source |
|---|---|---|
| `snapshot` (default) | live state: workspaces, in-flight, latest resources | runtime registry |
| `trace` | one flow as a span waterfall (events attached inline) | log |
| `events` | a flat, cross-trace stream of domain facts, by name/time | log |
| `cgroup` | resource series for a scope: cpu/mem/io **+ disk** | log |
| `layerstack` | layer inventory (leased / booked-by) + stack series (see side spec) | registry + disk/log |
| `raw` | matching NDJSON lines, for grep/jq | log |

### 7.1 `snapshot` — live current state (default; runtime registry)

```console
$ sandbox-cli observability snapshot --sandbox-id eos-abc
sandbox eos-abc   state ready

  workspaces
    ws-7   active    profile=default   layers=4

  in-flight executions            (from runtime registry, not the log)
    ns-42  namespace.exec.shell   trace req-9a1   running 7.3s   ws-7

  resources (latest)
    sandbox   cpu 12.3s   mem 41MB / 256MB
    ws-7      cpu  4.1s   mem 18MB        disk 1.2MB (320 files)
```

### 7.2 `trace` — one flow as a span waterfall (spans + attached events)

`--id` is the request's `request_id`; a flow-starting command **echoes it on
stderr**, and `--id last` resolves the most recent root trace.

```console
$ sandbox-cli exec --sandbox-id eos-abc "cargo build"
# trace: req-7f3
... command output ...

$ sandbox-cli observability trace --sandbox-id eos-abc --id req-7f3   # or: --id last
trace req-7f3   sandbox eos-abc   wall 4.33s   (call returned at 1.05s)

  +00.000  daemon.dispatch op=exec_command                 1051ms  ✓
  +00.002   └ command.exec one_shot                        1048ms  ✓
  +00.003      ├ workspace_session.create                    39ms  ✓
  +00.009      │   • lease.acquired r5
  +00.013      │   └ namespace.exec.mount_overlay            27ms  ✓
  +00.042      ├ namespace.exec.shell           [async]    4231ms  ✓ exit0   ← outlives call
  +00.055      │   └ namespace.runner.spawn_child            6ms  ✓   [Phase B: cross-process]
  +04.275      ├ workspace_session.capture_changes           11ms  ✓
  +04.287      ├ layerstack.publish r5→r6 +1 layer 40KB      12ms  ✓
  +04.300      └ workspace_session.destroy one_shot          25ms  ✓
  +04.320         • lease.released r5
```

### 7.3 `events` — flat domain-fact stream (by name / time)

Unlike `trace` (one flow as a tree), `events` is a **flat, cross-trace** stream —
for "show me every lease release" or "all errors." Filter with `--name` and
`--since-ms`. (`layerstack.publish` is a **span**, not an event — audit
publishes via `raw --kind span --name layerstack.publish`.)

```console
$ sandbox-cli observability events --sandbox-id eos-abc --name lease.released
events  sandbox eos-abc   name=lease.released   2 matched

  ts        trace     parent  attrs
  +04.320   req-7f3   d-8     revision=r5
  +18.122   req-9c2   d-31    revision=r7
```

### 7.4 `cgroup` — resource series for a scope (cpu/mem/io + disk)

`--scope` is `sandbox` (default) or a workspace id. Returns the per-scope `sample`
series: cgroup counters (cpu/mem/io, from `/sys/fs/cgroup`) **and** the disk
sample (upperdir bytes/files) carried in the same record; deltas at read.

```console
$ sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window-ms 60000
scope ws-1   window 60s   (Δ computed at read)

  t(+s)   cpu_total   Δcpu      mem_cur    disk        Δdisk
  00.0     1.00s        –       18.0MB     1.20MB        –
  10.0     4.10s     +3.10s     21.0MB     1.32MB     +120KB
  20.0     4.25s     +0.15s     20.5MB     1.32MB        +0
```

### 7.5 `layerstack` — layer inventory + stack stats

Layer inventory (leased / booked-by) and the stack time-series. Full examples in
`layerstack-impl.md` §4; the shapes:

```console
$ sandbox-cli observability layerstack --sandbox-id eos-abc                  # stack inventory (leased / booked-by)
$ sandbox-cli observability layerstack --sandbox-id eos-abc --workspace ws-7 # one session's lowers + private upper
$ sandbox-cli observability layerstack --sandbox-id eos-abc --samples --window-ms 60000   # stack time-series
```

### 7.6 `raw` — filtered NDJSON lines

Returns the matching log lines verbatim (newline-delimited), for grep/jq.

```console
$ sandbox-cli observability raw --sandbox-id eos-abc --trace req-7f3 --kind span
{"ts":1719500000042,"kind":"span","trace":"req-7f3","span":"d-2","parent":"d-1","name":"workspace_session.create","dur_ms":39.0,"status":"completed"}
{"ts":1719500001050,"kind":"span","trace":"req-7f3","span":"d-1","parent":"d-0","name":"command.exec","dur_ms":1048.0,"status":"completed","attrs":{"one_shot":true}}
{"ts":1719500004273,"kind":"span","trace":"req-7f3","span":"d-5","parent":"d-1","name":"namespace.exec.shell","dur_ms":4231.0,"status":"completed","attrs":{"exec_id":"ns-9","async":true,"exit_code":0}}
{"ts":1719500004286,"kind":"span","trace":"req-7f3","span":"d-6","parent":"d-1","name":"workspace_session.capture_changes","dur_ms":11.0,"status":"completed","attrs":{"one_shot":true}}
{"ts":1719500004299,"kind":"span","trace":"req-7f3","span":"d-7","parent":"d-1","name":"layerstack.publish","dur_ms":12.0,"status":"completed","attrs":{"base":"r5","revision":"r6","layers_added":1,"bytes":40960,"no_op":false}}
{"ts":1719500004325,"kind":"span","trace":"req-7f3","span":"d-8","parent":"d-1","name":"workspace_session.destroy","dur_ms":25.0,"status":"completed","attrs":{"one_shot":true}}
```

### 7.7 Empty / error states

```console
$ sandbox-cli observability trace --sandbox-id eos-abc --id nope
trace nope   sandbox eos-abc   (no records — unknown trace, or rotated out)

$ sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window 999999999
error: window_ms exceeds max (600000)
```

---

## 8. Removal checklist (mechanical; scoped greps must go to zero)

This rework covers **in-sandbox** observability. Host-side gateway/CLI timing is
retained (the in-sandbox `Sink` isn't reachable from the host process), so the
greps are **scoped to the in-sandbox crates**, not the whole workspace.

1. Delete the **5 in-sandbox** `timing.rs` + their `mod` decls: `sandbox-daemon`
   (`lib.rs:8`), `namespace-execution` (`lib.rs:9`), `namespace-process`
   (`lib.rs:12`), `operation` (`lib.rs:12`), `workspace` (`lib.rs:24`). **Retain**
   `sandbox-gateway/src/cli/timing.rs` (`cli/mod.rs:7`) and the gateway's host-side
   sites — out of scope.
2. Replace/delete the **~56 in-sandbox** call sites →
   `grep -rn 'timing::' crates/sandbox-runtime crates/sandbox-daemon` is empty.
   (Total `timing::` sites are ~77; the ~21 in `sandbox-gateway` stay.) Dotted
   labels carry over verbatim as span `name`s; return values are never used, so
   the swap is mechanical.
3. Delete `runtime_timing_env()` + its use
   (`sandbox-provider-docker/src/runtime.rs:88,113`) so timing env is no longer
   forwarded into the container → `grep -rn 'runtime_timing_env' crates` is empty.
   (`EOS_*_TIMING` remains a host-only gateway/CLI knob.)
4. Delete `src/store/**` + `rusqlite` → `grep -rn 'rusqlite' crates` is empty.
5. Collapse `sandbox-daemon/src/observability` to a thin caller: cgroup/disk
   readers move into the leaf crate; the SQLite store, the delta/counter caches,
   and the `namespace_execution_snapshots` replace/reconcile all go away.
   `collect()` emits `obs.sample` lines; the `snapshot` view is served straight
   from the live `observability_snapshot()` (no persistence, no reconcile —
   deltas are computed at read by the `Reader`).
6. Repoint the boundary test
   `operation/tests/observability_snapshot.rs` to the new leaf invariant (no
   protocol/runtime/daemon dep) or remove it.

**Untouched:** `layerstack/src/stack/projection/checkpoint.rs` — a legitimate
layer-projection domain concept, not timing.

---

## 9. Rollout

1. **Crate rework** (§6) — record types (one-record span), `Sink` (single-write
   append + rotation), `Observer`/`SpanGuard`/`SpanRegistry`/`TerminalHook`/`TraceContext`,
   `Reader`/history views, `paths.rs` retarget, move `collect/{cgroup,disk}`.
   Standalone, unit-tested; nothing consumes it yet.
2. **Daemon swap** — build `Observer`; `NoopObserver` → wire `SpanRegistry<exec_id>` as the engine's `TerminalHook`;
   `collect()` → emit `obs.sample`; serve the `snapshot` view from the live
   `observability_snapshot()`; generalize the RPC op + CLI.
3. **Replace the ~56 in-sandbox timing sites** with span guards / events, and
   **thread the trace id** (= `Request.request_id`): thread-local set at
   `dispatch_request`; the `SpanRegistry<exec_id>` wired as the engine's `TerminalHook`
   for the async exec span; the one-shot finalize closure captures the
   `TraceContext`. This in-process threading is what makes **Case A's async tail**
   (`d-5`, `d-6`, `lease.released`) correlate in Phase A.
4. **Layerstack facts** (`lease.acquired`/`lease.released` events; `layerstack.publish` span).
5. **Removal checklist** (§8) — the scoped greps gate the change.
6. **Phase B (follow-up):** thread `trace`/`parent` through
   `NamespaceRunnerRequest` (and optionally the daemon protocol) so the forked
   namespace-process (Case A's `np-0`) and gateway-initiated requests stitch into
   one cross-process tree; the watcher already carries the handle. **No schema
   change between phases** — the
   `trace`/`parent`/`span` fields exist from Phase A; Phase B only populates them
   across the fork.

---

## 10. Testing

- **Unit (crate):** record round-trip; **single-write append** keeps lines intact
  under N concurrent appenders (every line parses; none interleaved); the
  serialized-line cap truncates `attrs`, never the line; span ids are unique
  across simulated processes; `Reader` folds — trace tree + offsets from
  `ts - dur_ms`, pairwise sample deltas, raw filter; rotation emits the drop
  reader spans both files; empty trace output says "unknown trace, or rotated out."
- **Integration:** an `exec_command` reproduces Case A's shape (one record per
  span, `namespace.exec.shell` written on terminal under `req-7f3`, finalize
  `capture_changes` + `layerstack.publish` + `destroy` span + `lease.released`
  event share the trace); the `snapshot`
  view reflects live-registry in-flight (Case B) with **no** log dependency.
- **Fetch:** `get_observability` returns each `view`; `trace` the waterfall,
  `events` the flat stream, `cgroup` the series with deltas, `snapshot` from the
  live registry, `raw` the filtered lines — each selected by the matching
  `observability <view> --sandbox-id …` subcommand.
- **Gates:** the scoped removal greps return nothing (`timing::` empty in
  `sandbox-runtime` + `sandbox-daemon`; `rusqlite` empty; `runtime_timing_env`
  empty); `cargo build`, `cargo test`, `cargo clippy --all-targets` clean.
