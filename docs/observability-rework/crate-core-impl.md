# Crate Core — Record Model + Observer + Daemon Swap (drop SQLite)

Status: ready-to-implement.

This is the **implementation** spec for the crate-core slice of the observability
rework — the second vertical to ship after the layerstack slice
(`layerstack-impl.md`, landed). It covers the main spec's
(`README.md` §9) **Phases 1–2 plus the SQLite-removal half of the rollout**, in
three ordered parts:

- **Part 1 — Record model + Sink + Reader** (`sandbox-observability/src`):
  replace the four SQLite snapshot record types with the §3 `span`/`event`/`sample`
  envelope model, and generalize the samples-only `SampleSink`/`SampleReader`
  (seeded by the layerstack slice) into the full one-file `Sink`/`Reader`.
- **Part 2 — Observer emit API + config gate**: `Observer`, `SpanGuard`,
  `SpanRegistry`, `TerminalHook`/`NoopHook`, `TraceContext`, the emit methods,
  and an `observability.enabled` config gate. No runtime consumer yet — that is
  `span-trace-impl.md`.
- **Part 3 — Daemon swap + SQLite removal**: serve `snapshot`/`cgroup` from the
  **live runtime snapshot** (not the log), make `collect()` emit `obs.sample`
  lines, move the `cgroup`/`disk` readers into the leaf crate, and delete
  `store/**` + `rusqlite`.

Design source of truth: `README.md` (the rework model, §3 records, §5 seams, §6
crate rework, §7 fetch) and `cli-observability.md` (the CLI surface). This spec
says **what changes in code**.

> **Coexistence today.** The layerstack slice deliberately built a *parallel*
> NDJSON skeleton (`SampleSink`/`SampleReader` + `collect/layerstack.rs`) that
> runs **alongside the still-intact SQLite store** — exactly as `layerstack-impl.md`
> §3.5/§5 intended ("SQLite coexists until the main spec's removal phase"). This
> spec is that removal phase: it unifies the two into one file + one model and
> deletes SQLite.

---

## 1. What this slice changes (and what it does not)

| Lands here | Deferred |
|---|---|
| `Span`/`Event`/`Sample` record model (one record per span) | emitting spans/events from the runtime (`span-trace-impl.md`) |
| `Sink` (single-write append, line cap, rotation) | trace-id threading + instrumentation seams (`span-trace-impl.md`) |
| `Reader` folds: `trace` / `samples` / `raw` (`events` = `raw`+name) | layerstack/lease domain events (`span-trace-impl.md`) |
| `Observer`/`SpanGuard`/`SpanRegistry`/`TraceContext` + `TerminalHook`/`NoopHook` + config gate | runtime wiring + cross-process `np-*` spans (`span-trace-impl.md` / `removal-and-phaseb-impl.md`) |
| daemon `collect()` → `obs.sample`; `snapshot`/`cgroup` from live registry | — |
| delete `store/**` + `rusqlite`; `cgroup`/`disk` readers → leaf crate | — |

After this slice the crate has **one model, one file, no SQLite**, and the daemon
is a thin caller. Nothing in `sandbox-runtime` emits yet (it gains no obs
dependency here — that is the span phase).

---

## 2. Part 1 — Record model (`records.rs` reshape)

### 2.1 The shared envelope + three kinds

Replace the four snapshot record structs
(`SandboxSnapshotRecord`, `WorkspaceSnapshotRecord`,
`NamespaceExecutionSnapshotRecord`, `ResourceSampleRecord`,
`records.rs:24,63,114,156`) with the §3 model. One `serde` enum, **internally**
tagged on `kind` (`#[serde(tag = "kind")]` — the tag rides as a sibling field, e.g.
`{"kind":"span", …}`; *not* externally tagged, which would nest as `{"span":{…}}` and
break every example), so a single `Sink::append` / `Reader` scan handle all records:

```rust
// crates/sandbox-observability/src/record.rs   (rename records.rs → record.rs)
use std::borrow::Cow;
pub type Attrs = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Record {
    Span(Span),
    Event(Event),
    Sample(Sample),
}

pub struct Span {
    pub ts: i64,                 // completion time (unix ms); start = ts - dur_ms
    pub trace: String,
    pub span: String,            // process-unique "<proc>-<seq>"
    pub parent: Option<String>,
    pub name: Cow<'static, str>, // dotted label; &'static on write, owned on read
    pub dur_ms: f64,
    pub status: SpanStatus,      // completed | error | cancelled | timed_out (closed; §3.6)
    pub attrs: Attrs,            // domain facts: exit_code, op, one_shot, exec_id, async, …
}

pub struct Event {
    pub ts: i64, pub trace: String, pub parent: Option<String>,
    pub name: Cow<'static, str>, pub attrs: Attrs,
}

pub struct Sample {
    pub ts: i64,
    pub scope: String,           // "sandbox" | "stack" | "<workspace id>"
    pub metrics: Attrs,          // cpu_usec/mem_cur/disk_bytes/… or layer_count/layers_bytes/…
}
```

The envelope is just `ts` + `trace` (plus the `kind` tag and, for spans, the
`span`/`parent` ids). `trace` is omitted from `Sample` (samples are not part of a
flow). The current ad-hoc stack-sample JSON the layerstack slice writes
(`service.rs:append_stack_sample`, the `{ts, kind:"sample", scope:"stack", …}`
object) becomes a `Record::Sample` (`metrics` `#[serde(flatten)]`ed) — same bytes,
now typed.

- **No per-record `sandbox`/`component`/`pid`.** The file lives at one path per
  sandbox (`observability/observability.ndjson` under the daemon runtime dir), so
  `sandbox` is constant across every line — the host already selects it with
  `--sandbox-id` and the path encodes it. `component` and `pid` have **no** view that
  renders or filters them (verified against `cli-observability.md`); the `<proc>`
  token (`d-*` vs `np-*`) plus the span `name` (`daemon.dispatch` vs `command.exec`)
  already say where a record came from, so both are dropped. Stamp `sandbox` once at
  fetch/render if a shipped raw line ever needs it.
- **`exit_code` is an attr, not a field.** Only exec spans carry it; it would be
  absent on every `daemon.dispatch`/`workspace_session.create`/`session.*` span. Per the
  §3.6 open/closed contract, domain facts ride in `attrs` and only the cross-cutting
  `status` is a first-class field — so `exit_code` lives in `attrs`, and the renderer
  reads `attrs.exit_code` exactly as it reads `attrs.op`.
- **`name` is `Cow<'static, str>`, not `&'static str`.** Writes pass a `&'static`
  label with no allocation; reads (the `Reader` returns parsed records, §3.3)
  deserialize to owned. A plain `&'static str` cannot `Deserialize` from a transient
  buffer, so the records would be write-only — `Cow` lets the one struct serve both
  directions. The **emit API keeps `&'static str`** at the call site (a redaction
  guard: labels can never carry user input); dynamic operation identity rides in
  `attrs` (`attrs.op`), which is the open axis.
- **Records are write-internal.** Callers never construct a `Span`/`Event`/`Sample`;
  they call the emit API (§3.4), which stamps `ts`/`trace`/`span`/`parent`.

### 2.2 One line cap, no per-field validators

- **Delete** the six per-field length constants (`MAX_ID_LENGTH`, `MAX_KIND_LENGTH`,
  `MAX_OPERATION_LENGTH`, `MAX_ERROR_MESSAGE_LENGTH`, `MAX_SNAPSHOT_STATE_LENGTH`,
  `MAX_PATH_LENGTH`, `records.rs:3-8`), the per-struct `validate*` methods, and
  `RecordValidationError` (`records.rs:10-22,38-199`). They encoded the SQLite write
  contract (sandbox-id match, required columns); nothing reads them once the records
  are write-internal.
- **Add one** `MAX_LINE_BYTES` — the whole-serialized-line cap, the single bound the
  new model enforces (§3.2 / §4.2). The only externally-controlled field is
  `trace = request_id`; if it must be bounded independently, check it at emit, but the
  line cap already bounds the serialized record end-to-end. Six dead constants that no
  code reads buy nothing; one enforced cap is the honest contract.
- Add a `proc` token type for `<proc>-<seq>` span ids (§2.3).

### 2.3 Span-id uniqueness

A span id is `"<proc>-<seq>"` (`README.md` §3.1). `<proc>` is assigned **once per
process** so the daemon (`d-*`) and the forked namespace-process (`np-*`) never
collide on one file. The allocator is **one per process**, shared by every handle
(§3.4) — two independent allocators under the same `<proc>` would both emit `d-0`
and collide:

```rust
pub struct SpanIds { proc_token: &'static str, seq: AtomicU64 }
impl SpanIds {
    pub fn next(&self) -> String { format!("{}-{}", self.proc_token, self.seq.fetch_add(1, …)) }
}
```

`proc_token` is `record::proc::DAEMON` (`"d"`) for the daemon/runtime process and
`record::proc::NS` (`"np"`) for the namespace-process — named consts (§3.6), not bare
magic strings, so a typo cannot silently split span ids into a phantom proc. Because the
daemon and runtime share one process, they share one `SpanIds` (held in the one
per-process `Observer`, §3.4), so Case A's `daemon.dispatch` = `d-0` and `command.exec` =
`d-1` form one monotonic sequence across the daemon→runtime boundary (`README.md` §4.1).
Phase B (`removal-and-phaseb-impl.md`) relies on this split already existing.

---

## 3. Part 2 — `Sink`, `Reader`, `Observer`

### 3.1 `paths.rs` — one file

`ObservabilityPaths` (`paths.rs:11-56`) currently derives **two** paths:
`observability.sqlite` and `samples.ndjson`. Collapse to one:

| Before | After |
|---|---|
| `database_path` → `observability/observability.sqlite` | **deleted** |
| `samples_log_path` → `observability/samples.ndjson` | `log_path()` → `observability/observability.ndjson` |
| — | `rotated_log_path()` → `observability/observability.ndjson.1` (§4.3) |

`from_socket_path` (`paths.rs:20-39`) keeps deriving `observability_dir` from the
socket's parent; only the filenames change. `daemon_runtime_dir()` /
`observability_dir()` accessors stay.

### 3.2 `Sink` — generalize `SampleSink`

`SampleSink::append(&Value)` (`samples.rs:12-36`) already does the core trick:
serialize, append `\n`, open `O_APPEND|O_CREATE`, one `write_all`. Generalize it
to the typed `Record` and add the cap:

```rust
pub struct Sink { path: PathBuf }   // creates its parent dir once, at construction
impl Sink {
    pub fn append(&self, record: &Record) -> std::io::Result<()>;   // one write_all, incl. '\n'
}
```

- **Atomicity.** Unchanged from `SampleSink`: the whole line (incl. trailing `\n`)
  is built in one buffer and written with a **single `write_all`** to an
  `O_APPEND` fd. On the in-container fs the kernel serializes per-inode appends, so
  daemon (`d-*`) and namespace-process (`np-*`) lines never interleave — at any
  length (`README.md` §6).
- **Open once / no per-line `mkdir`.** `SampleSink` calls `create_dir_all` on every
  append (`samples.rs:25`); the parent dir is stable, so create it **once** in the
  constructor. `append` keeps opening the `O_APPEND` fd per line — robust to the
  daemon renaming the file at rotation (§4.3); cache the fd only if a profile shows
  `open()` dominating, and have the daemon reopen its `Sink` on rotation.
- **Line cap (deterministic).** Serialize once; if the line exceeds `MAX_LINE_BYTES`,
  replace the whole `attrs`/`metrics` with `{"_truncated": <original_byte_len>}` and
  re-serialize **once** — never drop `Map` entries (unpredictable order) and never
  loop. Only `attrs`/`metrics` are unbounded; the rest of the line is the small fixed
  envelope (`ts`/`trace`/`span`/`parent`/`name`), so the cap only ever touches
  `attrs`/`metrics`, and reader + human both see "this line was truncated," not a
  silently partial attr set. **Documented shape asymmetry (m9):** `Span.attrs` are
  nested, so the marker lands at `attrs._truncated`, while `Sample.metrics` are
  `#[serde(flatten)]`ed to the top level (this preserves the layerstack slice's on-disk
  bytes, §2.1), so its marker lands at the line's top level (`_truncated`). The reader
  extracts the marker per-kind; the asymmetry is documented rather than removed so the
  already-written flattened sample lines keep parsing.

### 3.3 `Reader` — generalize `SampleReader`

`SampleReader::samples(since)` (`samples.rs:39-68`) already does the forward scan +
JSON parse + skip-malformed + `ts` filter. Don't repeat that loop. Provide **one**
private scan primitive; the public views (`README.md` §6) are thin folds over it:

```rust
pub struct Reader { primary: PathBuf, rotated: PathBuf }

#[derive(Default)]
pub struct RawFilter {            // owned values + Default ⇒ `RawFilter { kind: Some("event".into()), ..Default::default() }`
    pub kind: Option<String>, pub name: Option<String>, pub trace: Option<String>, pub since_ms: i64,
}
impl Reader {
    // the ONE primitive: primary+rotated, parsed, malformed-skipped, ts-sorted;
    // the verbatim line is kept beside each record so `raw` needs no second pass.
    fn scan(&self) -> Vec<(Record, String)>;

    pub fn trace(&self, id: &str) -> Vec<SpanNode>;         // scan → filter trace==id → tree (no *View wrapper, m11)
    pub fn samples(&self, scope: &str, window_ms: i64) -> Vec<SampleDelta>;  // scan → filter → Δ
    pub fn raw(&self, filter: RawFilter) -> Vec<String>;    // scan → filter → re-emit the line
}
```

- **One scan, thin folds.** Each view is `scan()` + a filter; a future view ("spans
  slower than X", "errors only") is a one-line filter, not a new file-handling method.
  The sort is intrinsic: within a file `ts` is **not** monotonic (Case A appends `d-6`
  after its children, `README.md` §4.1), so every reader must sort by `ts` anyway —
  doing it once in `scan()` is strictly simpler.
- **`events` is a name-filtered fold over `scan()`, sharing `raw`'s filter shape.** The
  `events` view (`README.md` §7.3, `cli-observability.md` §3.4) selects `Event` records by
  `name` over the same `RawFilter { kind: Some("event".into()), name, .. }` —
  but it **reuses `scan()`'s already-parsed `Event` records** rather than re-JSON-parsing
  `raw`'s `Vec<String>` lines (m6): a thin fold like `trace`/`samples`, not a re-parse of a
  re-emit. `raw` stays the verbatim-line view; `events` hands back parsed records the CLI
  renders into its `ts / trace / parent / attrs` table without a second pass. The CLI
  presents newest-first; `scan()` is `ts`-ascending, so the CLI reverses (one place).
- `trace(id)`: filter by `trace`, build the tree by `span`/`parent`, order siblings
  by start (`ts - dur_ms`), offset each node by `(ts - dur_ms) - trace_start`;
  events render at their `ts` under `parent`. Resolves by id, never append order.
- `samples(scope, window_ms)`: filter by `scope` + `ts ≥ now - window_ms`, then pairwise
  Δ between adjacent same-scope samples (none stored). **Δ policy is generic** so a new
  metric is a one-line emit (§3.6): the Reader Δs the keys the **emitter tagged as
  counters** (monotonic — `cpu_usec`, `io_*bytes`, …) present in both samples, and leaves
  gauges/identity metrics (`mem_cur`, `mem_max`, `layer_count`) without a Δ. The
  counter/gauge tag lives at the **emit site** (the daemon owns its metric vocabulary and
  marks counter keys when it calls `sample`), **not** a leaf-side `const &[&str]` the leaf
  would have to keep in sync with daemon emit sites — so the Reader stays fully
  metric-agnostic (m10, §3.6). This is the **only** delta computation in the system; it
  backs the layerstack `--window-ms` trend (`service.rs:stack_trend`) and `cgroup` (§4.2).
- `raw(filter)`: re-emit the verbatim line kept by `scan()`, filtered by `kind`,
  `name`, `since_ms`, `trace`.

A trace that has aged out of both files simply has no records; the empty state reads
"no records — unknown trace, or rotated out" (`README.md` §7.7). No in-band sentinel
is written for rotation (§4.3) — it would force every fold to carry a 4th record kind
to distinguish two cases that single message already covers.

### 3.4 `Observer` — the emit API

The daemon and the runtime run in **one process** and must share one span-id
allocator and one thread-local context (else ids collide and the daemon→runtime
parent link breaks, `README.md` §4.1). So there is **one `Observer` per process** —
a cheap, `Clone` handle over an `Arc` core (sink + `SpanIds` + the thread-local
context). The daemon builds it; the runtime holds a clone (`span-trace-impl.md` §3).
There is **no** per-record `component`, so there is **no** component-tagged-handle
machinery: cloning the one `Observer` *is* the sharing.

```rust
struct Core { sink: Sink, ids: SpanIds, enabled: bool }   // one per process
thread_local! { static CTX: RefCell<Option<TraceContext>> = const { RefCell::new(None) }; }

#[derive(Clone)]
pub struct Observer { core: Arc<Core> }

pub struct ObserverConfig { pub proc: &'static str, pub enabled: bool }   // leaf-owned named gate;
                                                                         // daemon maps its sandbox-config
                                                                         // section + a record::proc const into it

impl Observer {
    pub fn new(config: ObserverConfig, sink: Sink) -> Self;   // builds the core (named gate, not a bare bool)

    pub fn span(&self, name: &'static str) -> SpanGuard;          // sync; nests under thread-local parent
    pub fn scope<T, E>(&self, name: &'static str, body: impl FnOnce(&SpanGuard) -> Result<T, E>) -> Result<T, E>;  // self-sets Error on Err before drop
    pub fn event(&self, name: &'static str, attrs: impl Into<Value>);          // nests under thread-local parent; drops if ctx is None
    pub fn sample(&self, scope: &str, metrics: impl Into<Value>);

    pub fn context(&self) -> Option<TraceContext>;               // current thread-local ctx
    pub fn with_context<R>(&self, ctx: impl Into<Option<TraceContext>>, f: impl FnOnce() -> R) -> R;  // scoped set + restore; accepts Option
}

pub struct TraceContext { pub trace: Arc<str>, pub parent: Option<Arc<str>> }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus { Completed, Error, Cancelled, TimedOut }
```

- **One `Observer`, cloned.** The daemon calls
  `Observer::new(ObserverConfig { proc: record::proc::DAEMON, enabled }, sink)`
  once and hands a clone to the runtime services (`span-trace-impl.md` §3). Both share
  the same `Sink`, the same `SpanIds`, and the same thread-local `CTX` (a free
  `thread_local!`, not a field — inherently process-shared). This is what makes
  `d-0`/`d-1` one sequence and lets a runtime span read the parent set by the daemon's
  `daemon.dispatch`.
- `TraceContext = { trace, parent }` uses `Arc<str>` — cloned on every `event`/`with_context`
  /registry `open`, and the trace id is `Request.request_id`; `Arc<str>` makes that
  clone a refcount bump, not a string copy. A **thread-local** holds the current
  context; `span()` reads it and pushes its own new span id as the parent for nested
  guards.
- `event` reads the thread-local parent (the enclosing span), so call sites are just
  `obs.event(name, json!({…}))` — no `ctx` argument to thread, and `json!(…)` (a `Value`)
  compiles because emit args take `impl Into<Value>` (M1). When the thread-local ctx is
  `None` (no enclosing span/context), `event` **drops the fact** rather than emitting an
  orphan `trace=""`/no-parent record (m6). The span-less explicit-parent case is
  `with_context(ctx, || obs.event(name, json!({…})))` — the blessed cross-thread pattern
  (`span-trace-impl.md` §7); there is **no** separate `event_in` producer (M5).

**Sync spans — `SpanGuard` (RAII, same-thread).** A guard accumulates `attrs` and a
`status` (default `Completed`) and writes **one** `Span` on drop:

```rust
pub struct SpanGuard { /* Option<OpenSpan>, Arc<Core>; status: Cell<SpanStatus>, attrs: RefCell<Attrs> */ }  // !Send
impl SpanGuard {
    pub fn attr(&self, key: &'static str, value: impl Into<Value>) -> &Self;   // annotate a live guard (op, one_shot, …)
    pub fn status(&self, status: SpanStatus) -> &Self;                         // override the default Completed (chainable)
}                                                                              // Drop ⇒ record(status, attrs)
```

`attr`/`status` are what let a sync span carry `daemon.dispatch op=…`,
`command.exec one_shot=true`, and — critically — record `status: error` when the
operation fails. For a fallible `Result`-returning seam, prefer the `Observer::scope`
combinator (§3.7): it runs the body and **self-sets `Error` on the `Err` before the guard
drops if the status is still `Completed`**, so a `?`/early-return cannot silently regress a
failed op to green and an explicit `TimedOut`/`Cancelled` is not clobbered. The chainable
`status` is then just the one-liner for an explicit `Err` arm
(`some_call().inspect_err(|_| span.status(Error))?`). Without these every sync span would
write `completed` with empty `attrs`, and the renderer would color failures green. A bare
`let _g = obs.span(name);` still works: it drops as `completed` with no attrs. (`scope`
covers the `Result`-returning sync seams only; `daemon.dispatch` returns a `Response`, not
a `Result`, so the combinator cannot apply there — that seam inspects the returned
`Response` and sets `status` by hand, which is `span-trace-impl.md`'s job, not this crate's.)

**Async spans — `SpanRegistry<K>` (park-by-id, record-by-id).** Async work
outlives the call and completes on another thread via an id-only callback (the
namespace exec, and future compaction/GC/prefetch). The reusable storage primitive is
**one** generic registry — so a new async source needs no bespoke map/lock:

```rust
pub struct SpanRegistry<K: Eq + Hash> { obs: Observer, open: Mutex<HashMap<K, OpenSpan>> }
struct OpenSpan { span: String, ctx: TraceContext, name: &'static str, start_ms: i64 }   // span = minted id
impl<K: Eq + Hash> SpanRegistry<K> {
    pub fn new(obs: Observer) -> Self;
    pub fn open(&self, id: K, ctx: TraceContext, name: &'static str) -> TraceContext;   // mint span id + park + self-stamp start_ms; returns child ctx { trace, parent: <new id> }
    pub fn launch<T, E>(&self, id: K, ctx: Option<TraceContext>, name: &'static str, f: impl FnOnce(Option<TraceContext>) -> Result<T, E>) -> Result<T, E>;   // open iff ctx Some, pass child ctx to f, cancel on Err
    pub fn record(&self, id: &K, status: SpanStatus, attrs: impl Into<Value>);   // pop + self-stamp end + write one Span
    pub fn cancel(&self, id: &K);                                       // pop without emitting (launch failed before run)
}                                                                        // Drop ⇒ record remaining as Cancelled (shutdown sweep)
```

- **No standalone `AsyncSpan` handle.** The only async shape that exists is the parked
  one (completion arrives via an id-only callback), so the registry owns the open span
  directly as plain data (`OpenSpan`). `open` **mints the span id and self-stamps
  `start_ms`**, stores both in `OpenSpan`, and **returns a child
  `TraceContext { trace, parent: <new id> }`** the caller threads into the forked child —
  so a cross-process child can stamp `parent = <this span id>` at *launch*, before the
  span completes (the canonical `np-0 parent=d-5`, `removal-and-phaseb-impl.md` §B.2; the
  id existing at launch is what makes that link constructible, M7). `open` takes a concrete
  `TraceContext`; callers that only have `Option<TraceContext>` use `launch`, which passes
  `Some(child_ctx)` to the launch closure when it parks a span and `None` when tracing is
  absent. `record`
  self-stamps the end and writes `dur_ms = now - start_ms`. **No caller ever passes a
  timestamp** — correct timing comes from *calling at the right moment*, not from
  threading a clock: the engine calls the terminal hook right after the work finishes and
  **before** any teardown (`span-trace-impl.md` §4), so `now` is the true completion
  instant. (A teardown failure therefore lands on the teardown's own span, not this one —
  the intended attribution.)
- **`cancel` for launch failure, folded into `launch`.** `open` parks a span and stamps
  its start, so a launch that fails *after* `open` but before the work runs must `cancel`
  (pop, no emit) — otherwise the shutdown sweep would later emit a bogus `cancelled` for a
  span that never ran. The `launch` combinator folds this three-step dance: it opens (iff
  `ctx` is `Some`), runs `f`, and `cancel`s internally on `Err`, so the caller never writes
  the cancel by hand and a forgotten `cancel`/`open` can't leak or drop a span
  (`span-trace-impl.md` §4). With `launch` covering the production launch path, `open`/`cancel`
  stay low-level escape hatches for nonstandard handoffs only (M3).
- **Drop is a shutdown sweep, not a per-span net.** A registry lives for the process,
  so its `Drop` (recording leftovers as `cancelled`) only fires at teardown. A watcher
  that panics before recording leaks its entry until then; that is acceptable for a
  best-effort observability log, but it means the sweep is a backstop for clean
  shutdown, **not** a guarantee that every started span gets a record. Bound map growth
  by always pairing `open` with exactly one `record`/`cancel` (which `launch` guarantees).

**The engine-facing interface — `TerminalHook<K>` + `NoopHook`.** An async engine
should not know about spans; it accepts a generic hook it notifies at the terminal
edge. The name stays out of the `Observer` family (it is a single terminal-edge callback
that never mentions a span, C2). That interface is **owned by the obs leaf** (so any
execution-type crate can share it) and is deliberately minimal — one method:

```rust
pub trait TerminalHook<K>: Send + Sync {
    fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>);
}

pub struct NoopHook;
impl<K> TerminalHook<K> for NoopHook { fn on_terminal(&self, _: &K, _: SpanStatus, _: Option<i64>) {} }

// The registry is itself the hook — no per-source adapter (m1):
impl<K: Eq + Hash> TerminalHook<K> for SpanRegistry<K> {
    fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>) {
        // folds exit_code into attrs + stamps async:true, then calls record(id, status, attrs)
    }
}
```

- **Only the terminal edge.** With one-record-at-completion, observability needs *only*
  `on_terminal`; "what's running" is the engine's own live registry (Case B), not the
  hook. So there is no `on_running` (the old `ExecutionObserver` had one; it becomes
  dead here — dropped) and no timestamp — the impl records the span when `on_terminal` is
  called, which the engine arranges to be the true completion moment
  (`span-trace-impl.md` §4).
- **`NoopHook` is generic** (`impl<K>`), so it serves any engine's `K`. It and the
  `TerminalHook<K>` trait move here from `namespace-execution` (`types.rs:19,30`); the
  engine swap is `span-trace-impl.md` §4 — it binds `K`, which the leaf must not know.
  With the blanket `impl<K> TerminalHook<K> for SpanRegistry<K>` above, the engine wires
  its `Arc<SpanRegistry<NamespaceExecutionId>>` **directly** as the hook — the old
  `NamespaceExecutionObserver` adapter collapses to (at most) its one domain attr
  (`exec_id`) or disappears (m1).
- **A different execution type** wires its own `SpanRegistry<ItsId>` as the hook directly
  (via the blanket impl), or a thin impl over it if it needs source-specific terminal
  attrs; the leaf does not change. The generality lives in `<K>` (the right axis: each
  source has a distinct id type and its own mutex), not in a per-source enum.
- **`exit_code: Option<i64>` is a documented pragmatic universal.** It is an exec-only
  datum: a non-exec async source (compaction/GC) passes `None` forever, and the namespace
  impl re-inserts it into `attrs`. Keep it on the trait as the one optional terminal code,
  and route any second consumer's source-specific terminal payload through that impl's
  `record` attrs rather than a widened trait (m8). The engine's `Ok` outcome maps to
  `SpanStatus::Completed` via a `to_span_status` bridge — the single intentional vocabulary
  difference between the engine's outcome enum and the on-disk `status`, documented as a
  bridge, not an accident (m8).

**General contracts:**
- **Never fail the observed op.** When `enabled == false`, every method is a near-free
  no-op (guards do nothing on drop). When enabled, all `Sink` errors are swallowed and
  over-long lines truncate (§3.2). Emit never returns an error to the caller.
- **Redaction.** `name`s are `&'static str` at the call site (cannot carry user input).
  `attrs`/`metrics` are the open channel and MUST NOT carry raw command lines, env, or
  secret-bearing paths — the file ships to the host over the RPC. They are bounded by
  the line cap; redaction of their *contents* is the call site's responsibility.

### 3.5 Config gate

There is no observability config section today; `DaemonObservability::from_config`
(`service.rs:70-86`) enables itself implicitly when `sandbox_id`+`socket_path` are
present. Add an explicit gate, following the `ConfigDocument::section`
typed-schema pattern (`sandbox-config/src/document.rs:57-75`):

```rust
// crates/sandbox-config/src/configs/observability.rs   (new)
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityConfig {
    #[serde(default = "default_true")] pub enabled: bool,
    #[serde(default = "default_max_file_bytes")] pub max_file_bytes: u64,   // rotation threshold (§4.3)
}
```

- **Default on in-sandbox.** The in-container daemon reads
  `doc.section::<ObservabilityConfig>("observability")` (missing section → enabled)
  and passes `enabled` into the process `Observer::new` (and thus every clone).
- **Off for the host CLI.** The host process never constructs an in-sandbox
  `Observer` (it only ever uses the `Reader` over a fetched file), so emit stays
  off there with no flag.
- **Tunable retention.** `max_file_bytes` defaults to today's value but is config, so
  retention can track event volume/variety without a recompile. With one rotation
  (§4.3) the file holds ≈ `2 × max_file_bytes`; size it so a max-window query
  (`MAX_RESOURCE_WINDOW_MS`, 600 s) stays answerable, else old samples read as
  "rotated out." The crate stays `sandbox-config`-free — the daemon reads the value
  and passes it in.

### 3.6 Extending the model — the open/closed contract

The model is a generic transport: a new **variety** of operation/event/metric should
be a **new call site, not a crate change**. What is open vs. closed:

| Axis | Open (no crate change) | Closed (touches the crate) |
|---|---|---|
| span   | `name`, `attrs` (incl. `exit_code`), nesting via `parent` | `SpanStatus` enum |
| event  | `name`, `attrs` — fully open | — |
| sample | `scope`, every `metrics` key, and its counter/gauge tag (set at emit) — fully open | — |
| trace  | any participant joins by sharing `trace` + `parent` | single `parent` ⇒ tree, not DAG |
| async source | wire a `SpanRegistry<ItsId>` as `TerminalHook<K>` (blanket impl) | — |

Four conventions keep the open axes open and the closed ones honest:

1. **New span/event/sample = a new `name`/`scope` string at the seam.** A new sync op
   is `obs.span("workspace.gc")`; a new fact is `obs.event("oom.killed", json!({…}))`;
   a new metric source is `obs.sample("gpu-0", json!({…}))`; a new metric on an existing
   scope is one more key in `collect()`. None touch the crate — contrast the old
   `ResourceSampleRecord`, where one field meant a migration + struct field + validator
   + row + read query.
2. **`SpanStatus` stays the closed cross-cutting axis** —
   `completed|error|cancelled|timed_out`, the success/failure dimension the renderer
   color-codes. Domain sub-states (`skipped`, `degraded`, retries) and `exit_code` go in
   `attrs`, **not** a new enum arm or field. Add an arm only if the renderer must treat a
   new outcome universally (rare).
3. **Metric counter/gauge semantics are tagged at the emit call, not in the leaf
   (m10).** The `metrics` map makes metric *names* open but erases the counter/gauge
   distinction the old typed fields encoded. Rather than a leaf-side `COUNTERS` const that
   must stay in sync with the daemon's emit sites — a leaf change the open/closed table
   denies, and a coupling of the leaf to the daemon's metric vocabulary — the **emitter
   tags which keys are counters** at the `sample` call (the daemon owns the vocabulary). A
   suffix rule (`*_bytes`) is **not** enough — `io_read_bytes` (counter) and
   `mem_cur`/`disk_bytes` (gauge-ish) collide — so the tag is explicit at emit, not
   inferred. The Reader Δs the emitter-tagged counters (§3.3) and leaves gauges/identity
   metrics without a Δ; the leaf stays metric-agnostic and a new metric is genuinely one
   new call site.
4. **New async source = wire a `SpanRegistry<ItsId>` as `TerminalHook<K>`.** Because the
   engine-facing interface is generic over the id and the storage is the generic
   `SpanRegistry<K>`, instrumenting a new async subsystem is wiring the registry itself as
   the hook (blanket impl m1), not a crate change (§3.4).

**Known ceiling — trace is a tree, not a DAG.** `parent: Option<String>` cannot
express fan-in (one coalesced `layerstack.publish` serving N sessions belongs under N
traces). Today such a flow picks one trace and emits `event`s into the others. If
multi-parent flows become common, that is a deliberate model change (OpenTelemetry-
style span *links*: `links: Vec<{trace, parent}>`) — flag it, don't pre-build it.

**Name discoverability + label grammar.** The open `name` vocabulary has no central
registry, so at many varieties typos and sprawl creep in. Add an additive `record::names`
module of `pub const` `&'static str` labels — grep-able and typo-safe, with zero
constraint on extensibility (a new name is still one new const). These strings are
user-facing grep/jq targets in an append-only file, so state **one grammar rule** next to
`record::names`: **spans = `subsystem[.area].action` (imperative); events =
`subsystem.fact` (past-tense)** — e.g. spans `command.exec`, `workspace_session.create`,
`namespace.exec.mount_overlay`, `layerstack.publish`; events `lease.acquired`,
`lease.released` (the span name follows the same span/op split that the span
`workspace_session.create` vs the op `create_workspace_session` already uses — the op identity persists in
`attrs.op`/`operation_name`). Beside it, add `record::proc` consts (`DAEMON = "d"`,
`NS = "np"`) so the `<proc>` token (§2.3) is a named const, not a bare magic string a typo
could split into a phantom proc (m7).

### 3.7 API reference — the method surface

Every timestamp is self-stamped by the `Core`; **no caller passes a clock**. There are
three producers — `span`/`scope` (sync, RAII), `SpanRegistry::open`/`launch`/`record`
(async, keyed), and `event`/`sample` (point-in-time) — and two plumbers — `with_context`
(set the chain) and `context` (snapshot it to cross a thread). There is **no** `event_in`:
a span-less explicit-parent emit is `with_context(ctx, || obs.event(name, attrs))` (M5).

**`TraceContext`** (value object): `trace: Arc<str>` (= `Request.request_id`),
`parent: Option<Arc<str>>` (the span id a child attaches under; `None` at the root).

**`Observer`** (`Clone`, one per process):

| Method | Args | Returns | Writes? | Parent / scope |
|---|---|---|---|---|
| `new` | `config: ObserverConfig, sink: Sink` | `Observer` | — | builds the `Core` (sink + `SpanIds`); `config` carries the proc token + the named gate |
| `span` | `name: &'static str` | `SpanGuard` | on **drop** | thread-local parent; pushes its own id |
| `scope` | `name: &'static str, body: impl FnOnce(&SpanGuard) -> Result<T, E>` | `Result<T, E>` | on **drop** | thread-local parent; self-sets `Error` on `Err` before drop only if status is still `Completed` |
| `event` | `name: &'static str, attrs: impl Into<Value>` | `()` | **now** | thread-local parent; drops if ctx is `None` |
| `sample` | `scope: &str, metrics: impl Into<Value>` | `()` | **now** | no `trace`/`parent` |
| `context` | — | `Option<TraceContext>` | — | snapshot the thread-local ctx |
| `with_context` | `ctx: impl Into<Option<TraceContext>>, f: impl FnOnce() -> R` | `R` | — | set thread-local for `f`, restore after |

**`SpanGuard`** (sync, `!Send`, ends on drop, same thread):

| Method | Args | Returns | Effect |
|---|---|---|---|
| `attr` | `key: &'static str, value: impl Into<Value>` | `&Self` | annotate a fact on a live guard (chainable) |
| `status` | `status: SpanStatus` | `&Self` | override the default `Completed` (chainable) |
| *(Drop)* | — | — | write one `Span`: `dur_ms = now − start`, accumulated `status` + `attrs` |

> **`span()` must be let-bound (m3).** `attr`/`status` annotate a *live* guard; they do not
> construct one. `obs.span("x").attr(…);` compiles but immediately drops the temporary,
> recording a ~0 ms span — bind it (`let span = obs.span("x"); span.attr(…);`) so the guard
> lives for the scope it measures. An optional clippy `let_underscore`/temporary-drop lint
> guards the footgun.

**`SpanRegistry<K: Eq + Hash>`** (async store, the reusable primitive):

| Method | Args | Returns | Writes? | Effect |
|---|---|---|---|---|
| `new` | `obs: Observer` | `SpanRegistry<K>` | — | empty registry sharing the `Core` |
| `open` | `id: K, ctx: TraceContext, name: &'static str` | `TraceContext` | — | mint span id + park; self-stamp start; return child ctx `{ trace, parent: <new id> }` (M7) |
| `launch` | `id: K, ctx: Option<TraceContext>, name: &'static str, f: impl FnOnce(Option<TraceContext>) -> Result<T, E>` | `Result<T, E>` | on `record` | open iff `ctx` is `Some`, pass child ctx to `f`, `cancel` on `Err` (M3) |
| `record` | `id: &K, status: SpanStatus, attrs: impl Into<Value>` | `()` | **now** | pop + write one `Span`; `dur_ms = now − start` |
| `cancel` | `id: &K` | `()` | — | pop without writing (launch failed before run) |
| *(Drop)* | — | — | `record` leftovers as `Cancelled` (shutdown sweep) |

> `launch` is the normal launch path because it makes register+cancel atomic and exposes
> the child context Phase B needs. Use `open`/`cancel` directly only for nonstandard handoffs.

**Engine hook — `TerminalHook<K>` (trait) and its impls** (the engine swap lives with its
source, `span-trace-impl.md` §4):

| Type | Method | Args | Effect |
|---|---|---|---|
| `trait TerminalHook<K>` | `on_terminal` | `id: &K, status: SpanStatus, exit_code: Option<i64>` | the terminal edge the engine calls; no timestamp |
| `NoopHook` | `on_terminal` | *(same)* | no-op (`impl<K>`, any engine) |
| `SpanRegistry<K>` | `on_terminal` | *(same)* | blanket impl: folds `exit_code` into attrs, stamps `async:true`, calls `record` (m1) |

**Read side — `Reader`:**

| Method | Args | Returns | View |
|---|---|---|---|
| `trace` | `id: &str` | `Vec<SpanNode>` | one flow as a span forest (events attached by `parent`); the lone `*View` wrapper dropped (m11) |
| `samples` | `scope: &str, window_ms: i64` | `Vec<SampleDelta>` | per-scope series with read-time Δ |
| `raw` | `filter: RawFilter` | `Vec<String>` | verbatim lines; the `events` view folds `scan()`'s parsed `Event` records by name (no re-parse, m6) |

---

## 4. Part 3 — Daemon swap + SQLite removal

### 4.1 `collect()` → `obs.sample`; live `snapshot`/`cgroup`

`DaemonObservability` (`service.rs:27-34`) holds `store: ObservabilityStore` plus
two delta caches (`resource_counters`, `disk_samples`). Reshape it to hold an
`Observer` (or `Sink`) over `log_path()` and **no store**:

| Method | Today | After |
|---|---|---|
| `collect()` (`service.rs:88`) | `write_snapshot()` → SQLite upsert/reconcile/replace + `append_stack_sample` | for each scope (`sandbox`, each `<ws>`, `stack`) read counters and `obs.sample(scope, metrics)`; nothing else |
| `write_snapshot` + `*_record` builders (`service.rs:166-366`) | build records, upsert/reconcile/replace into SQLite | **deleted** |
| `read_snapshot_value` (`service.rs:150-164`) | `store.read_observability_snapshot` → JSON | **deleted**; replaced by live reshape |
| `resource_deltas` + `resource_counters`/`disk_samples` caches (`service.rs:368-406`) | compute + cache deltas at write | **deleted**; deltas computed at read by `Reader::samples` |
| `append_stack_sample` / `stack_trend` (`service.rs:107,139`) | `SampleSink`/`SampleReader` | fold into `obs.sample` / `Reader::samples("stack", …)` |

- **`snapshot` view** (`view.rs:snapshot_view_response:112-131`,
  `live_snapshot:154-161`): drop the `read_snapshot_value` (SQLite) call; reshape
  `operations.observability_snapshot()` (the live runtime registry — already used
  by the layerstack inventory) into the snapshot JSON, joined with the **latest
  `Sample` per scope** from `Reader`. No log dependency for entity state /
  in-flight; only the "resources (latest)" rows read the latest sample.
- **`cgroup` view** (`view.rs:cgroup_view_response:133-150`,
  `resource_series_for_scope:165-179`): serve from `Reader::samples(scope,
  window_ms)` with deltas at read, for `scope` = `sandbox` or a `<ws>`. This
  generalizes the existing stack-only `--window-ms` trend to any scope.
- The cgroup/disk **counters** still come from `/sys/fs/cgroup` + the upperdir
  walk — those readers move to the leaf crate (§4.2).

### 4.2 Move `cgroup`/`disk` readers into the leaf crate

`README.md` §6 puts the resource readers in `sandbox-observability/src/collect/`
(the layerstack slice already moved `collect/layerstack.rs` there). Move the daemon's
two pure readers next to it:

| From | To | Note |
|---|---|---|
| `sandbox-daemon/src/observability/cgroup.rs` (`CgroupSample::read(&Path)`) | `sandbox-observability/src/collect/cgroup.rs` | already a pure `&Path → struct` |
| `sandbox-daemon/src/observability/disk.rs` (`sample_upperdir(&Path)`) | `sandbox-observability/src/collect/disk.rs` | already pure; budgeted DFS |

`collect()` calls them and packs the results into `Sample.metrics`
(`cpu_usec`/`mem_cur`/`mem_max`/`disk_bytes`/`files`/…). The daemon keeps only the
orchestration (which scopes to sample, when), not the readers.

### 4.3 Rotation (daemon-owned, no marker)

A soft size cap (`ObservabilityConfig::max_file_bytes`, §3.5) bounds the file; on
exceed, the **daemon** rotates `observability.ndjson` → `observability.ndjson.1`
(replacing any prior `.1`) and reopens its `Sink`. The `Reader` reads both, ordered
by `ts` (§3.3). Rotation drops the oldest history; a trace that has aged out simply has
no records, which the empty state renders as "no records — unknown trace, or rotated
out" (`README.md` §7.7). **No in-band marker record is written** — a 4th record kind
would have to be threaded through every `Reader` fold to distinguish "rotated out" from
"never existed," a distinction the empty-state message already makes. Rotation is checked
in `collect()` (the daemon's periodic tick), not on the hot append path. Size the cap so
`2 × max_file_bytes` covers `MAX_RESOURCE_WINDOW_MS` (§3.5), or a max-window query reads
partly rotated-out.

### 4.4 Delete SQLite

| Delete | Where |
|---|---|
| `src/store.rs`, `src/store/{read,schema,rows}.rs` (9 migrations, `ObservabilityStore`, Row types, `StoreError`) | `sandbox-observability/src/store/**` |
| `rusqlite` dependency | `sandbox-observability/Cargo.toml` |
| `rusqlite` **dev**-dependency + SQLite test helpers | `sandbox-daemon` (`tests/unit/observability.rs`) |
| the 4 snapshot record types + Row exports | `records.rs`, `lib.rs` (`pub use store::{…}`, the snapshot-record re-exports) |
| `snapshot_record` + `bound_*` helpers (dead once snapshot is live) | `sandbox-daemon/src/observability/namespace_execution.rs` |
| SQLite schema/introspection tests | `sandbox-observability/tests/{schema.rs,support/mod.rs}` |

`lib.rs` exports shrink to: `paths`, `record` (`Record`/`Span`/`Event`/`Sample` +
`SpanStatus` + `Attrs` + `MAX_LINE_BYTES` + `record::names` const labels + `record::proc`
consts, §3.6), `Sink`, `Reader` (+ `SpanNode`/`SampleDelta`/`RawFilter`),
`Observer`/`ObserverConfig`/`SpanGuard`/`SpanRegistry`/`TraceContext`,
`TerminalHook`/`NoopHook`, `collect::{sample_layerstack, cgroup, disk}`. The
records are write-internal (§2.1); only `Reader` view outputs and the emit API are
caller-facing.

### 4.5 Retire the old snapshot op

`get_observability_snapshot` (`dispatch.rs:8`,
`PRIVATE_OBSERVABILITY_SNAPSHOT_OP`, handler
`dispatch_private_observability_snapshot:106-132`) is the SQLite-backed
predecessor of `get_observability view=snapshot`. **Audit its callers** (manager /
status paths) and repoint them to `get_observability` with `view:"snapshot"`, then
delete the op + handler. If a caller can't migrate in this slice, leave the op as a
thin alias that calls `snapshot_view_response` (no SQLite) and delete it in
`removal-and-phaseb-impl.md`.

---

## 5. Boundary

- `sandbox-observability` stays a **leaf**: `serde`, `serde_json`, `thiserror`
  only — no `rusqlite`, no `protocol`/`runtime`/`daemon`/`config`. The
  `TerminalHook<K>` trait is generic over the id type, so owning it in the leaf pulls
  no dependency (the consumer supplies `K`). The daemon reads `ObservabilityConfig`:
  `enabled` plus a `record::proc` const become the leaf-owned `ObserverConfig`
  (proc + gate) passed into `Observer::new`; `max_file_bytes` remains daemon-owned rotation
  policy. The crate never imports `sandbox-config`.
- `tests/dependency_guard.rs` (forbids `sandbox-runtime`/`sandbox-daemon`/
  `sandbox-manager`) is unchanged and still passes. Add `rusqlite` to its forbidden
  list to lock the removal.
- The runtime gains **no** obs dependency in this slice; the operation boundary
  test (`operation/tests/observability_snapshot.rs:91-95`) still passes untouched.

---

## 6. Rollout (ordered)

1. **Record model** (§2) — `records.rs → record.rs`, `Record`/`Span`/`Event`/
   `Sample`, `SpanStatus`, `name: Cow<'static, str>` (no `sandbox`/`component`/`pid`;
   `exit_code` in `attrs`), `record::names` labels (§3.6), one `MAX_LINE_BYTES`, drop
   validators, add `SpanIds`. Unit-tested; nothing consumes it.
2. **`paths.rs` + `Sink` + `Reader`** (§3.1–§3.3) — one file, single-write append
   + line cap + rotation, the folds (`trace`/`samples`/`raw`; `events` = `raw`+name).
   Generalizes the landed `SampleSink`/`SampleReader`.
3. **`Observer` + interfaces + config gate** (§3.4–§3.5) — one process `Observer`
   (Arc core) + thread-local context, `SpanGuard` (attr/status), `SpanRegistry<K>`
   (open/launch/record/cancel), the generic `TerminalHook<K>` + `NoopHook` (+ blanket
   impl for `SpanRegistry<K>`), `ObservabilityConfig`. Standalone; unit-tested with a fake
   `K`.
4. **Daemon swap** (§4.1–§4.3) — `collect()` emits `obs.sample`; `snapshot`/
   `cgroup` from live registry + `Reader`; move `cgroup`/`disk` readers; rotation.
5. **SQLite removal** (§4.4–§4.5) — delete `store/**` + `rusqlite`; retire the old
   op; the scoped grep gates the change.

Steps 1–3 are crate-only; 4–5 are the daemon. No throwaway — every piece carries
the system to the span phase (`span-trace-impl.md`).

---

## 7. Testing

- **Record:** round-trip each `Record` variant through `serde` (internally tagged
  on `kind` — the tag is a sibling field), including read-back into an owned `name`
  (`Cow`); `exit_code` round-trips as an attr; the stack-sample JSON from the layerstack
  slice still parses.
- **Sink:** single-write append keeps lines intact under N concurrent appenders
  (every line parses; none interleaved) — port the layerstack append test to the
  typed `Sink`; an over-`MAX_LINE_BYTES` line becomes `{"_truncated": n}`, never a
  split line; rotation renames + reopens and the `Reader` spans both files ordered by
  `ts`.
- **Reader:** the views fold over one `scan()`; `trace` builds the tree + offsets from
  `ts - dur_ms` and resolves a child event whose record precedes its parent span;
  `samples` Δs only the emitter-tagged counter keys (a gauge added to `metrics` gets no Δ);
  `raw` re-emits the verbatim line filtered by kind/name/trace/since, and the `events` view
  folds `scan()`'s parsed `Event` records by name (no re-parse); `RawFilter` defaults
  (`..Default::default()`).
- **Observer:** disabled → no file writes, no panics; `SpanGuard` writes one record
  on drop with `start = ts - dur_ms`, carries `attr(...)`, and records `error` after
  `status(Error)` (and via the `scope` combinator on an `Err` body); `event` with no
  thread-local ctx drops the fact (no orphan record); **two cloned `Observer`s on one
  core** share the `SpanIds` (no duplicate `d-0`) and the thread-local context (a runtime
  span nests under a daemon-set parent); span ids unique across the
  `record::proc::{DAEMON, NS}` tokens.
- **SpanRegistry / TerminalHook:** `open` mints the id and returns the child `TraceContext`;
  `open` then `record` writes one `Span` with `dur_ms = now - start` (recorded at the
  `record` call); `launch` cancels on an `Err` body and records on `Ok`; `record` from
  another thread works (the `Sink` serializes); `cancel` emits nothing; the `Drop` sweep
  records a leaked entry as `cancelled`; `NoopHook` writes nothing for any `K`, and the
  blanket `SpanRegistry<K>` hook folds `exit_code`/`async:true` into the span.
- **Daemon:** `snapshot`/`cgroup` views render from the live registry + `Reader`
  with **no** SQLite; `collect()` writes `obs.sample` lines for `sandbox`/`<ws>`/
  `stack`.
- **Gates:** `grep -rn rusqlite crates` → **zero**; `dependency_guard` (incl. new
  `rusqlite` rule) green; `cargo build`, `cargo test`, `cargo clippy --all-targets`,
  `cargo fmt` clean.
