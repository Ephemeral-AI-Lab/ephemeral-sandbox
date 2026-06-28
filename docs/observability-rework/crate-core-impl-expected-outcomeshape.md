# Crate Core Expected Outcome Shape

Source: `docs/observability-rework/crate-core-impl.md`

This is a compact shape checklist for the crate-core observability rework. It
does not restate the implementation order. It names the expected file/module
layout and the struct field changes after the slice lands.

## 1. High-level target

After this slice:

- `sandbox-observability` has one record model: `Record::{Span, Event, Sample}`.
- Observability writes one append-only NDJSON file per sandbox.
- SQLite storage is gone.
- Daemon snapshot/cgroup views are served from live runtime state plus latest/read-time samples.
- The daemon emits samples through the same `Observer`/`Sink` path used by spans/events.
- The runtime does not gain a direct observability dependency in this slice.

## 2. Runtime file shape

### Before

```text
<daemon-runtime-dir>/observability/
  observability.sqlite
  samples.ndjson
```

### After

```text
/eos/runtime/daemon/observability/
  observability.ndjson
  observability.ndjson.1
```

Notes:

- In sandbox containers, `<daemon-runtime-dir>` is `/eos/runtime/daemon`.
- Do not place daemon runtime or observability files under `/tmp`.
- `observability.ndjson` is the primary log.
- `observability.ndjson.1` is the single rotated log.
- No in-band rotation marker record is written.
- `Reader` reads primary plus rotated and sorts by `ts`.

## 3. Source tree shape

### `crates/sandbox-observability/src`

Expected outcome:

```text
crates/sandbox-observability/src/
  collect/
    cgroup.rs
    disk.rs
    layerstack.rs
  lib.rs
  paths.rs
  record.rs
  reader.rs
  sink.rs
  observer.rs
```

The exact file split can be smaller if the code stays readable, but the public
shape should be equivalent.

### Renamed

| Before | After |
|---|---|
| `records.rs` | `record.rs` |

### Generalized

| Before | After |
|---|---|
| `samples.rs::SampleSink` | `Sink` |
| `samples.rs::SampleReader` | `Reader` |

### Moved into the leaf crate

| Before | After |
|---|---|
| `crates/sandbox-daemon/src/observability/cgroup.rs` | `crates/sandbox-observability/src/collect/cgroup.rs` |
| `crates/sandbox-daemon/src/observability/disk.rs` | `crates/sandbox-observability/src/collect/disk.rs` |

### Deleted from `sandbox-observability`

```text
crates/sandbox-observability/src/store.rs
crates/sandbox-observability/src/store/
crates/sandbox-observability/tests/schema.rs
crates/sandbox-observability/tests/support/
```

Also remove:

- `rusqlite` dependency from `sandbox-observability`.
- `rusqlite` dev-dependency and SQLite helpers from daemon tests.
- SQLite row exports from `lib.rs`.

## 4. `ObservabilityPaths` shape

### Before

```rust
pub struct ObservabilityPaths {
    daemon_runtime_dir: PathBuf,
    observability_dir: PathBuf,
    database_path: PathBuf,
    samples_log_path: PathBuf,
}
```

Methods:

```rust
database_path()
samples_log_path()
daemon_runtime_dir()
observability_dir()
```

### After

```rust
pub struct ObservabilityPaths {
    daemon_runtime_dir: PathBuf,
    observability_dir: PathBuf,
    log_path: PathBuf,
    rotated_log_path: PathBuf,
}
```

Methods:

```rust
daemon_runtime_dir()
observability_dir()
log_path()
rotated_log_path()
```

Expected filenames:

```rust
log_path         = observability_dir.join("observability.ndjson")
rotated_log_path = observability_dir.join("observability.ndjson.1")
```

Removed:

```rust
database_path()
samples_log_path()
```

## 5. Record model shape

### Old structs removed

Remove these snapshot/storage records:

```rust
SandboxSnapshotRecord
WorkspaceSnapshotRecord
NamespaceExecutionSnapshotRecord
ResourceSampleRecord
RecordValidationError
```

Remove old validation constants:

```rust
MAX_ID_LENGTH
MAX_KIND_LENGTH
MAX_OPERATION_LENGTH
MAX_ERROR_MESSAGE_LENGTH
MAX_SNAPSHOT_STATE_LENGTH
MAX_PATH_LENGTH
```

Replace them with one serialized-line cap:

```rust
MAX_LINE_BYTES
```

### New model

```rust
pub type Attrs = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Record {
    Span(Span),
    Event(Event),
    Sample(Sample),
}
```

### `Span`

```rust
pub struct Span {
    pub ts: i64,
    pub trace: String,
    pub span: String,
    pub parent: Option<String>,
    pub name: Cow<'static, str>,
    pub dur_ms: f64,
    pub status: SpanStatus,
    pub attrs: Attrs,
}
```

Field meaning:

| Field | Meaning |
|---|---|
| `ts` | completion time, unix ms |
| `trace` | trace/request id |
| `span` | process-unique id, `"<proc>-<seq>"` |
| `parent` | parent span id |
| `name` | static dotted label on write, owned on read |
| `dur_ms` | span duration |
| `status` | closed status enum |
| `attrs` | open domain facts such as `op`, `one_shot`, `exit_code` |

### `Event`

```rust
pub struct Event {
    pub ts: i64,
    pub trace: String,
    pub parent: Option<String>,
    pub name: Cow<'static, str>,
    pub attrs: Attrs,
}
```

### `Sample`

```rust
pub struct Sample {
    pub ts: i64,
    pub scope: String,
    pub metrics: Attrs,
}
```

`Sample` has no `trace`; samples are not part of a flow.

### Fields intentionally absent

These are not top-level record fields:

```rust
sandbox
component
pid
exit_code
```

Reason:

- `sandbox` is encoded by the per-sandbox log path.
- `component`/`pid` are not part of any planned view/filter.
- `exit_code` is span-specific and lives in `Span.attrs.exit_code`.

## 6. Old record field mapping

### `SandboxSnapshotRecord`

Before:

```rust
sandbox_id
state
workspace_root
daemon_runtime_dir
socket_path
pid_path
daemon_pid
sampled_at_unix_ms
error_message
```

After:

- No replacement record.
- Snapshot entity state comes from the live runtime snapshot.
- Latest resource values come from latest `Sample` records.

### `WorkspaceSnapshotRecord`

Before:

```rust
sandbox_id
workspace_id
state
network_profile
workspace_root
upperdir
workdir
namespace_fd_count
base_root_hash
layer_count
sampled_at_unix_ms
error_message
```

After:

- No replacement record.
- Workspace state comes from the live runtime snapshot.
- Resource/layer metrics move to `Sample.metrics`.

### `NamespaceExecutionSnapshotRecord`

Before:

```rust
sandbox_id
namespace_execution_id
workspace_session_id
operation
lifecycle_state
sampled_at_unix_ms
error_message
```

After:

- No replacement snapshot record.
- Completed async work is represented as a `Span`.
- Domain facts such as execution id and operation live in `Span.attrs`.

### `ResourceSampleRecord`

Before:

```rust
sample_id
sandbox_id
workspace_id
sampled_at_unix_ms
cgroup_path
cgroup_available
cgroup_error
cpu_usage_usec
cpu_usage_delta_usec
sample_delta_ms
memory_current_bytes
memory_current_delta_bytes
memory_max_bytes
memory_max_unlimited
disk_upperdir_bytes
disk_upperdir_delta_bytes
disk_file_count
disk_dir_count
disk_symlink_count
disk_truncated
disk_read_error_count
disk_first_error_path
```

After:

```rust
Sample {
    ts,
    scope,
    metrics,
}
```

Expected metric keys include, but are not limited to:

```text
cpu_usec
mem_cur
mem_max
disk_bytes
files
layer_count
layers_bytes
active_leases
```

Deltas are no longer stored. `Reader::samples(scope, window_ms)` computes deltas
at read time for metrics tagged as counters by the emitter.

## 7. Span identity shape

Add:

```rust
pub struct SpanIds {
    proc_token: &'static str,
    seq: AtomicU64,
}
```

Expected id format:

```text
<proc>-<seq>
```

Named proc constants:

```rust
pub mod proc {
    pub const DAEMON: &str = "d";
    pub const NS: &str = "np";
}
```

The daemon/runtime process uses one shared `SpanIds` through one cloned
`Observer`, so ids are monotonic across daemon and runtime spans.

## 8. Sink shape

### Before

```rust
SampleSink::append(&serde_json::Value)
```

### After

```rust
pub struct Sink {
    path: PathBuf,
}

impl Sink {
    pub fn append(&self, record: &Record) -> std::io::Result<()>;
}
```

Behavior:

- Create the parent directory once at construction.
- Serialize the whole record once.
- Append one complete line with one `write_all`.
- Enforce `MAX_LINE_BYTES`.
- If too large, replace `attrs` or `metrics` with a truncation marker and serialize once more.

Truncation marker shape:

```json
{"_truncated": 12345}
```

For `Span`, marker lives under `attrs._truncated`.

For flattened `Sample` metrics, marker lives at the line top level.

## 9. Reader shape

### Before

```rust
SampleReader::samples(since)
```

### After

```rust
pub struct Reader {
    primary: PathBuf,
    rotated: PathBuf,
}

#[derive(Default)]
pub struct RawFilter {
    pub kind: Option<String>,
    pub name: Option<String>,
    pub trace: Option<String>,
    pub since_ms: i64,
}
```

Methods:

```rust
impl Reader {
    fn scan(&self) -> Vec<(Record, String)>;

    pub fn trace(&self, id: &str) -> Vec<SpanNode>;
    pub fn samples(&self, scope: &str, window_ms: i64) -> Vec<SampleDelta>;
    pub fn raw(&self, filter: RawFilter) -> Vec<String>;
}
```

Expected scan behavior:

- Read primary plus rotated logs.
- Parse valid records.
- Skip malformed lines.
- Keep verbatim line beside each parsed record.
- Sort by `ts`.

`events` is not a separate file scan. It is a name-filtered fold over parsed
`Event` records from `scan()`.

## 10. Observer and context shape

### Core structs

```rust
struct Core {
    sink: Sink,
    ids: SpanIds,
    enabled: bool,
}

#[derive(Clone)]
pub struct Observer {
    core: Arc<Core>,
}

pub struct ObserverConfig {
    pub proc: &'static str,
    pub enabled: bool,
}

pub struct TraceContext {
    pub trace: Arc<str>,
    pub parent: Option<Arc<str>>,
}
```

Thread-local context:

```rust
thread_local! {
    static CTX: RefCell<Option<TraceContext>> = const { RefCell::new(None) };
}
```

### `SpanStatus`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    Completed,
    Error,
    Cancelled,
    TimedOut,
}
```

### `Observer` methods

```rust
impl Observer {
    pub fn new(config: ObserverConfig, sink: Sink) -> Self;

    pub fn span(&self, name: &'static str) -> SpanGuard;
    pub fn scope<T, E>(
        &self,
        name: &'static str,
        body: impl FnOnce(&SpanGuard) -> Result<T, E>,
    ) -> Result<T, E>;
    pub fn event(&self, name: &'static str, attrs: impl Into<Value>);
    pub fn sample(&self, scope: &str, metrics: impl Into<Value>);

    pub fn context(&self) -> Option<TraceContext>;
    pub fn with_context<R>(
        &self,
        ctx: impl Into<Option<TraceContext>>,
        f: impl FnOnce() -> R,
    ) -> R;
}
```

Important shape rules:

- There is one `Observer` per process, cloned into users.
- There is no component-tagged observer handle.
- `event` drops if there is no current context.
- Emit methods swallow sink errors and must not fail the observed operation.

## 11. `SpanGuard` shape

```rust
pub struct SpanGuard {
    // Option<OpenSpan>, Arc<Core>, status, attrs, previous context.
}

impl SpanGuard {
    pub fn attr(&self, key: &'static str, value: impl Into<Value>) -> &Self;
    pub fn status(&self, status: SpanStatus) -> &Self;
}
```

Drop behavior:

- Write one `Span`.
- Compute `dur_ms = now - start`.
- Restore the previous thread-local context.
- Never panic from best-effort emit.

`SpanGuard` is same-thread and must remain `!Send`.

## 12. Async span registry shape

```rust
pub struct SpanRegistry<K: Eq + Hash> {
    obs: Observer,
    open: Mutex<HashMap<K, OpenSpan>>,
}

struct OpenSpan {
    span: String,
    ctx: TraceContext,
    name: &'static str,
    start_ms: i64,
}
```

Methods:

```rust
impl<K: Eq + Hash> SpanRegistry<K> {
    pub fn new(obs: Observer) -> Self;

    pub(crate) fn open(
        &self,
        id: K,
        ctx: TraceContext,
        name: &'static str,
    ) -> TraceContext;

    pub fn launch<T, E>(
        &self,
        id: K,
        ctx: Option<TraceContext>,
        name: &'static str,
        f: impl FnOnce(Option<TraceContext>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        K: Clone;

    pub fn record(&self, id: &K, status: SpanStatus, attrs: impl Into<Value>);

    pub(crate) fn cancel(&self, id: &K);
}
```

Shape rules:

- No standalone `AsyncSpan` handle.
- `open` and `cancel` are crate-private.
- `launch` is the public launch path; it requires `K: Clone` (parks the id, retains it to `cancel` on `Err`).
- `record` pops by id and writes one completed async span.
- `Drop` records remaining open spans as `Cancelled`.

## 13. Engine hook shape

```rust
pub trait TerminalHook<K>: Send + Sync {
    fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>);
}

pub struct NoopHook;

pub trait SpanKeyAttrs {
    fn write_attrs(&self, attrs: &mut Attrs);
}
```

Expected impls:

```rust
impl<K> TerminalHook<K> for NoopHook {
    fn on_terminal(&self, _: &K, _: SpanStatus, _: Option<i64>) {}
}

impl<K: Eq + Hash + SpanKeyAttrs> TerminalHook<K> for SpanRegistry<K> {
    fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>) {
        // async:true, optional exit_code, id.write_attrs(...), then record(...)
    }
}
```

No per-source adapter type is expected.

## 14. Daemon observability shape

### Before

```rust
pub struct DaemonObservability {
    sandbox_id: String,
    paths: ObservabilityPaths,
    store: ObservabilityStore,
    next_sample_id: AtomicU64,
    disk_samples: Mutex<HashMap<DiskCacheKey, CachedDiskSample>>,
    resource_counters: Mutex<HashMap<ResourceScopeKey, PreviousResourceCounters>>,
}
```

Supporting structs to remove:

```rust
DiskCacheKey
CachedDiskSample
ResourceScopeKey
PreviousResourceCounters
ResourceDeltas
```

### After

```rust
pub struct DaemonObservability {
    sandbox_id: String,
    paths: ObservabilityPaths,
    observer: Observer,
    max_file_bytes: u64,
    rotate_lock: Mutex<()>,
}
```

`observer` can be represented as a `Sink` only if no span/event API is needed at
that layer yet, but the intended shape is `Observer`. `rotate_lock` serializes
`rotate_if_needed` so two concurrent `collect()` ticks can't both rename and
clobber freshly rotated history (the size is re-checked under the lock).

### Method shape changes

| Method/area | Before | After |
|---|---|---|
| `collect()` | writes SQLite snapshot and stack sample | emits `obs.sample(...)` for sandbox/workspace/stack scopes |
| `write_snapshot()` | builds rows and upserts SQLite | deleted |
| `read_snapshot_value()` | reads SQLite snapshot JSON | deleted |
| `resource_deltas()` | caches write-time deltas | deleted |
| `append_stack_sample()` | writes via `SampleSink` | folded into `obs.sample("stack", metrics)` |
| `stack_trend()` | reads via `SampleReader` | folded into `Reader::samples("stack", ...)` |

## 15. View serving shape

### Snapshot view

Before:

```text
SQLite snapshot rows -> JSON response
```

After:

```text
operations.observability_snapshot()
  + latest Sample per scope from Reader
  -> snapshot JSON response
```

Entity state and in-flight work are not reconstructed from the log.

### Cgroup view

Before:

```text
SQLite ResourceSampleRecord rows with stored deltas
```

After:

```text
Reader::samples(scope, window_ms)
```

Deltas are computed at read time.

## 16. Config shape

Add:

```text
crates/sandbox-config/src/configs/observability.rs
```

Expected type:

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
}
```

Config ownership:

- `sandbox-config` owns deserialization.
- Daemon reads this section.
- `sandbox-observability` stays config-free.
- Daemon maps `enabled` plus `record::proc::DAEMON` into `ObserverConfig`.
- Daemon keeps `max_file_bytes` as rotation policy.

## 17. Public export shape

`crates/sandbox-observability/src/lib.rs` should export roughly:

```rust
pub mod collect;
pub mod paths;
pub mod record;

pub use collect::{sample_layerstack, LayerBytes, LayerStackBytes};
pub use paths::{ObservabilityPathError, ObservabilityPaths};
pub use record::{
    Attrs, Event, Record, Sample, Span, SpanStatus, MAX_LINE_BYTES,
};
pub use reader::{RawFilter, Reader, SampleDelta, SpanNode};
pub use sink::Sink;
pub use observer::{
    NoopHook, Observer, ObserverConfig, SpanGuard, SpanKeyAttrs, SpanRegistry,
    TerminalHook, TraceContext,
};
```

And should no longer export:

```rust
ObservabilityStore
StoreError
ObservabilitySnapshotRows
Observability*Row
SandboxSnapshotRecord
WorkspaceSnapshotRecord
NamespaceExecutionSnapshotRecord
ResourceSampleRecord
RecordValidationError
SampleSink
SampleReader
```

## 18. Dependency boundary

`sandbox-observability` allowed dependencies:

```text
serde
serde_json
thiserror
```

Forbidden dependencies:

```text
rusqlite
sandbox-daemon
sandbox-runtime
sandbox-manager
sandbox-config
protocol
```

Add `rusqlite` to the dependency guard forbidden list.

## 19. Deletion checklist

Delete or retire:

- SQLite store module and migrations.
- SQLite row types.
- SQLite schema/introspection tests.
- Old snapshot record builders.
- `namespace_execution::snapshot_record` and `bound_*` helpers once dead.
- `get_observability_snapshot` private op, if all callers can move to `get_observability view="snapshot"`.

If a caller cannot move in this slice, keep the old op only as a thin alias to
the new live `snapshot` view, with no SQLite path.

## 20. Minimal final check

Expected gates after the slice:

```text
rg -n "rusqlite" crates
cargo fmt
cargo build
cargo test
cargo clippy --all-targets
```

`rg -n "rusqlite" crates` should return no live dependency or code usage.
