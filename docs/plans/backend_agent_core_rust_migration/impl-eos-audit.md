# impl-eos-audit — audit event envelope, synchronous bus, JSONL writer, deterministic redaction

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §13.

## 1. Purpose & Responsibility (SRP)

`eos-audit` owns the **write-only audit side channel**: the structured event
envelope (`AuditEvent` + correlation `AuditNode`), the `AuditSink` trait and the
synchronous in-process `AuditEventBus`, the append-only JSONL writer, the
deterministic + testable **redaction/summary** helpers, and the neutral
constructors that translate tool-lifecycle stream data into engine-sourced audit
rows.

It must **NOT**:
- depend on any downstream crate. Per anchor §5, `eos-audit` is *referenced-by*
  `eos-tools`, `eos-engine`, `eos-workflow`, `eos-plugin-catalog`, `eos-runtime`;
  it depends only on `eos-types`. In particular it does **not** import the
  engine's `ToolExecution*` stream events — that would create
  `eos-audit → eos-engine → eos-audit` (see GC-audit-05).
- own lifecycle policy, decide *when* events fire, or know tool/workflow
  semantics. Producers populate the identifiers they already know; the collector
  never infers missing IDs from payload text (preserved invariant from
  `audit/base.py`).
- buffer, batch, lane-route, sample, or run any async/daemon ring. Daemon-side
  buffering (`sandbox.daemon.audit_buffer`, lanes) is **out of scope** — that is
  deep-sandbox machinery the plan keeps separate.

## 2. Dependencies

- **Upstream crates (depends on):** `eos-types` only (newtype IDs, `UtcDateTime`,
  `Clock` trait, `JsonObject`, `CoreError` for `#[from]` if needed). See
  impl-eos-types.md §5.
- **Downstream consumers (used by):** `eos-tools`, `eos-engine`, `eos-workflow`,
  `eos-plugin-catalog`, `eos-runtime` (anchor §5).

External crates (pinned through `[workspace.dependencies]`, inherited with
`{ workspace = true }` — `proj-workspace-deps`):

| Crate | Why | rust-skills |
|---|---|---|
| `serde` (derive) | `Serialize`/`Deserialize` on every wire/DTO type (event, node, payload, sources). | anchor §9 (`api-serde-optional` n/a — serde is mandatory here) |
| `serde_json` | Canonical JSON encoding for digest/byte-size + JSONL row serialization. Must support `sort_keys`-equivalent + compact separators. | parity (GC-audit-02) |
| `schemars` | `JsonSchema` on serialized types for the Phase 0 schema-snapshot parity harness. | anchor §11 |
| `thiserror` | The single crate error enum (`AuditError`). | `err-thiserror-lib` |
| `sha2` | `Sha256` digest of canonical-JSON bytes (`sha256:<hex>`). | parity (GC-audit-02) |
| `hex` (or `sha2`'s formatter) | lowercase hex encoding of the digest. | parity |

No `tokio`, no `async-trait`, no `futures` — the bus is **synchronous** (GC-audit-03).
The crate is runtime-agnostic; it neither spawns nor blocks on a runtime.

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `audit/base.py` — `AuditNode`, `AuditEvent`, `AuditSink`, `AuditSource`, `NoopAuditSink` | `node.rs`, `event.rs`, `sink.rs` | Moves wholesale. `JsonValue = Any` → `JsonObject` / `serde_json::Value`. `AuditSource` Literal → enum. Add `schema_version` to the serialized row (new). |
| `audit/bus.py` — `AuditEventBus`, `AuditDispatchError`, `AuditHandler` | `bus.rs` | Moves. `BaseException` capture → `Result`-based error stashing (GC-audit-04). Dynamic `subscribe`/unsubscribe-handle dropped unless a caller needs it (YAGNI); sinks registered at construction. |
| `audit/jsonl.py` — `append_jsonl_event`, `_json_default` | `jsonl.rs` | Moves. Generic `Mapping` arg → concrete `&AuditEvent` (YAGNI). Double `ts` stamping collapsed to one Clock-set `ts` (GC-audit-01). |
| `engine/audit/stream.py` — `audit_events_from_stream_event`, `_node_from_stream`, `_shape`, `_redacted_shape`, `_digest`, `_encoded_size`, `_json_bytes`, metadata remap | `redaction.rs` (`_shape`/`_redacted_shape`/`_digest`/`_encoded_size`/canonical bytes) + `engine_stream.rs` (neutral row constructors) | `_shape`/redaction/digest move verbatim. The translation **dispatch over `ToolExecution*` types stays in `eos-engine`**; `eos-audit` exposes neutral constructors it calls (GC-audit-05). |
| `engine/audit/events.py` — `TOOL_STARTED/COMPLETED/FAILED` | `engine_stream.rs` consts | Move the 3 type strings. |
| `plugins/core/loader.py` `_install_plugin_audit_shim` + `sandbox/daemon/audit_schema.py` `PluginSection`/`build_plugin_event` | `plugin.rs` | Owns the `plugin.*` event **family + PluginSection payload shape** (GC-audit-06). Only the inner `payload["plugin"]` section is byte-compatible with `build_plugin_event`; the `AuditEvent` envelope (`source = AuditSource::Sandbox`, `node`) is a **design decision, not Python parity** — the daemon dict has no `source`/`node` (see §6 plugin.rs). The *wrapping of `tool.execute`* belongs to `eos-plugin-catalog` (it calls these constructors). Lane routing / `safe_emit` / daemon ring are **out of scope**. |

**In scope:** event/node types, sink trait, sync bus, JSONL append, redaction
summaries, neutral tool-row + plugin-row constructors, `schema_version`.

**Out of scope:** `message/events.py` `ToolExecution*` (owned by `eos-engine`),
daemon audit buffer/lanes, all non-tool/non-plugin daemon sections
(`LayerStackSection`, `OccSection`, `OverlayWorkspaceSection`,
`IsolatedWorkspaceSection`, `BackgroundToolSection`, `ToolCallSection`,
`OsResourceSection`, `DaemonSection`) — those are deep-sandbox, kept separate.

## 4. File & Module Layout

```
src/
  lib.rs            // pub use re-exports (proj-pub-use-reexport); crate docs (//!)
  error.rs          // AuditError (single thiserror enum)
  node.rs           // AuditNode + builder
  event.rs          // AuditEvent, AuditSource, schema_version, serialized row
  sink.rs           // AuditSink trait, NoopAuditSink
  bus.rs            // AuditEventBus, AuditDispatchError
  jsonl.rs          // JsonlSink (append-only writer)
  redaction.rs      // shape/redacted-shape/digest/encoded-size, canonical JSON bytes
  engine_stream.rs  // neutral tool_started/tool_completed constructors + type consts
  plugin.rs         // PluginSection + plugin.* event constructors
```

`lib.rs` re-exports the public surface; redaction internals that are not part of
the contract are `pub(crate)` (`proj-pub-crate-internal`) except the deterministic
helpers a downstream wants to call directly (digest/encoded-size), which are `pub`.

## 5. Contracts Owned Here

Per anchor §5, `eos-audit` owns `AuditEvent`, `AuditNode`, the **`AuditSink`
trait**, `AuditEventBus`, the JSONL writer, and redaction. Full field shapes are
in §6; signatures below.

### `AuditSink` (trait — owned)

Synchronous, object-safe **without** `#[async_trait]` (the seam is sync per
directive; anchor §6 lists no async rule for it). Stored as `Arc<dyn AuditSink>`
at the composition root.

```rust
/// Write-only audit side channel. Implementations must not panic; recoverable
/// failures are reported through `AuditError` so the bus can isolate them.
pub trait AuditSink: Send + Sync {
    /// # Errors
    /// Returns `AuditError` when the sink cannot persist the event (e.g. io).
    fn publish(&self, event: &AuditEvent) -> Result<(), AuditError>;
}
```

(`own-slice-over-vec`/borrow: `&AuditEvent`, not owned.) `NoopAuditSink::publish`
returns `Ok(())`. Sealing is **not** applied — external test sinks/JSONL/in-memory
are first-class implementors (this is the OCP seam).

### `AuditEventBus` (owned)

Synchronous single-process fanout. Owns a `Vec<Arc<dyn AuditSink>>` and a
`Mutex<Vec<AuditDispatchError>>`. `publish` is **infallible to its caller**: it
visits every sink, stashing each `Err` (GC-audit-04). See §6/§8.

### JSONL writer (`JsonlSink` / `BufferedJsonlSink`, owned)

`JsonlSink` appends one untruncated canonical JSON object per line and is useful
for tests and low-volume compatibility. Production wiring should prefer
`BufferedJsonlSink`: `publish` sends the event into a bounded sync channel, and a
single writer thread owns the open append-mode file handle. If the queue is full,
`publish` returns an `AuditError::Backpressure` so the bus can record the failure
instead of blocking a Tokio worker thread.

Contracts **referenced** (not redefined here): `RequestId`, `WorkflowId`,
`IterationId`, `AttemptId`, `TaskId`, `AgentRunId`, `SandboxId`, `ToolUseId`,
`UtcDateTime`, `Clock`, `JsonObject`, `CoreError` — all owned by `eos-types`
(impl-eos-types.md §5). `ToolName` is owned by `eos-tools` (see §6 note — stored
here as `String` to avoid the downstream edge).

## 6. Types, Fields & Schemas

### `AuditNode` (`node.rs`)

Correlation envelope. Derives `Debug, Clone, PartialEq, Default, Serialize,
Deserialize, JsonSchema`. `#[non_exhaustive]` (`api-non-exhaustive`) with a
builder/`Default` so callers set only known IDs. All fields `Option<_>`,
serialized with `skip_serializing_if = "Option::is_none"` to match Python's
omit-when-None shape.

| Field | Rust type | serde/schemars | Source of truth |
|---|---|---|---|
| `request_id` | `Option<RequestId>` | skip-if-none | `audit/base.py` |
| `workflow_id` | `Option<WorkflowId>` | skip-if-none | base.py |
| `iteration_id` | `Option<IterationId>` | skip-if-none | base.py |
| `attempt_id` | `Option<AttemptId>` | skip-if-none | base.py |
| `task_id` | `Option<TaskId>` | skip-if-none | base.py |
| `agent_name` | `Option<String>` | skip-if-none | base.py — **label, not an ID; stays `String`** |
| `agent_run_id` | `Option<AgentRunId>` | skip-if-none | base.py |
| `sandbox_id` | `Option<SandboxId>` | skip-if-none | base.py |
| `tool_name` | `Option<String>` | skip-if-none | base.py — **`ToolName` is owned downstream (`eos-tools`); kept `String` to avoid `eos-audit → eos-tools` edge (GC-audit-05)** |
| `tool_use_id` | `Option<ToolUseId>` | skip-if-none | base.py |

### `AuditSource` (`event.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditSource { Workflow, Engine, Sandbox, LiveE2e }
```

Renames to the exact Python `Literal` strings: `"workflow"`, `"engine"`,
`"sandbox"`, `"live_e2e"` (`#[serde(rename = "live_e2e")]` on `LiveE2e`).
`type-enum-states`, `anti-stringly-typed`.

### `AuditEvent` (`event.rs`)

Derives `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`.
`#[non_exhaustive]` + a `new(...)` constructor that sets `ts` from an injected
`&dyn Clock` (GC-audit-01).

| Field | Rust type | serde/schemars | Source of truth |
|---|---|---|---|
| `schema_version` | `u32` | always serialized, value `1` | **new** (plan §13 "Add `schema_version`") |
| `source` | `AuditSource` | — | base.py |
| `type` (Rust field `event_type`, `#[serde(rename = "type")]`) | `String` | event type string (e.g. `"engine.tool.started"`) | base.py — `type` is a Rust keyword |
| `node` | `AuditNode` | flattened? **no** — keep nested under `"node"` to match Python | base.py |
| `payload` | `JsonObject` | default empty map | base.py (`Mapping[str, JsonValue]`) |
| `correlation_id` | `Option<String>` | skip-if-none | base.py |
| `ts` | `UtcDateTime` | RFC3339 / epoch per eos-types serde; single source | base.py default factory **+** jsonl double-stamp collapsed (GC-audit-01) |

`event_type` stays a `String` (not a `ToolName`/enum): event-type strings span
engine/plugin/workflow namespaces (`engine.tool.started`, `plugin.error`, …) and
producers own them. Constants for the engine + plugin families live in their
constructor modules.

```rust
impl AuditEvent {
    #[must_use]
    pub fn new(
        source: AuditSource,
        event_type: impl Into<String>,
        node: AuditNode,
        payload: JsonObject,
        clock: &dyn Clock,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION, // 1
            source,
            event_type: event_type.into(),
            node,
            payload,
            correlation_id: None,
            ts: clock.now(),
        }
    }
}
```

### Tool started/completed audit row shapes (`engine_stream.rs` payloads)

These are the **payload `JsonObject` shapes** the neutral constructors build,
matching `engine/audit/stream.py` exactly (GC-audit-02). Event types:
`TOOL_STARTED = "engine.tool.started"`, `TOOL_COMPLETED = "engine.tool.completed"`,
`TOOL_FAILED = "engine.tool.failed"`; `source = AuditSource::Engine`.

**Started row payload** (`build_tool_started`):

| key | value | derivation |
|---|---|---|
| `tool_name` | tool name string | `node.tool_name` |
| `tool_use_id` | tool-use id string | `node.tool_use_id` |
| `status` | `"ok"` | constant |
| `input_shape` | shaped input (`_shape`: dict→typed-keys, list→first 5 elems' type names) | `redaction::shape(&input)` |
| `input_redacted` | dict keys with `"<redacted>"` values / list of `"<redacted>"` (first 5) / scalar `"<redacted>"` | `redaction::redacted_shape(&input)` |
| `input_digest` | `"sha256:<hex>"` of canonical JSON | `redaction::digest(&input)` |
| `input_bytes` | byte length of canonical JSON | `redaction::encoded_size(&input)` |

**Completed/failed row payload** (`build_tool_completed`):

| key | value | derivation |
|---|---|---|
| `tool_name` | tool name string | `node.tool_name` |
| `tool_use_id` | tool-use id string | `node.tool_use_id` |
| `status` | `"error"` if `is_error` else `"ok"` | input |
| `error_kind` | `"tool_result_error"` if `is_error` else explicit `Value::Null` (key always present) | input |
| `output_shape` | `_shape(output)` (output is a `&str` → scalar type name `"str"`) | redaction |
| `output_digest` | `"sha256:<hex>"` | redaction |
| `output_bytes` | canonical byte length | redaction |
| `is_error` | bool | input |
| `is_terminal` | bool (**terminal metadata**) | input |
| `metadata` | `metadata` with inner `timings` remapped to `domain_timings` | `audit_metadata_from(metadata)` |
| `timings` | `{}` (separate empty object) | constant |

Constructor signatures take **neutral borrowed inputs** owned here/`eos-types`,
NOT engine stream types (GC-audit-05):

```rust
pub const TOOL_STARTED: &str = "engine.tool.started";
pub const TOOL_COMPLETED: &str = "engine.tool.completed";
pub const TOOL_FAILED: &str = "engine.tool.failed";

#[must_use]
pub fn tool_started(node: AuditNode, input: &JsonObject, clock: &dyn Clock) -> AuditEvent;

#[must_use]
pub fn tool_completed(
    node: AuditNode,
    output: &str,
    is_error: bool,
    is_terminal: bool,
    metadata: &JsonObject,
    clock: &dyn Clock,
) -> AuditEvent;
```

`eos-engine` owns the dispatch over its `ToolExecutionStartedEvent` /
`ToolExecutionCompletedEvent` and the `_node_from_stream` ID-precedence logic
(`_first_text`/`_text_or_none`), calling these constructors with already-resolved
fields. Node assembly (which IDs win between event + metadata) is engine policy.

The `tool_name`/`tool_use_id` payload keys are sourced from `node.tool_name` /
`node.tool_use_id` (the constructors take no separate tool params). **Parity
nuance:** Python's payload uses the raw `stream_event.tool_name`/`tool_use_id`,
whereas the node fields are `_text_or_none`-stripped and `_first_text`
fallback-resolved in `_node_from_stream`, so the engine must resolve these into
the node *before* calling — empty-string / metadata-fallback cases otherwise
diverge from the Python rows. `tool_completed` selects
`event_type = TOOL_FAILED` when `is_error` else `TOOL_COMPLETED` (stream.py
line 48), and inserts `error_kind` as `serde_json::Value::Null` (not skip/omit)
when not an error, so the golden JSONL carries the explicit `null` key like
Python's `json.dumps`.

### Redaction (`redaction.rs`) — deterministic & testable (GC-audit-02)

Pure, side-effect-free functions over `&serde_json::Value`:

- `shape(&Value) -> Value`: dict → `{stringified-key: shape(v)}`; list/tuple →
  first **5** elements mapped through `shape`; scalar → its type name string,
  pinned to mirror Python's `type(value).__name__`: `str → "str"`, `int → "int"`,
  `float → "float"`, `bool → "bool"`, `None → "NoneType"`. Because `serde_json`
  has a single `Value::Number`, the impl must split `is_i64()||is_u64() → "int"`
  vs `is_f64() → "float"` to reproduce Python's int/float distinction. Nailed by
  golden test.
- `redacted_shape(&Value) -> Value`: dict → every key with `"<redacted>"`;
  list → up to 5 `"<redacted>"`; scalar → `"<redacted>"`.
- `digest(&Value) -> String`: `format!("sha256:{hex}")` of `Sha256` over
  `canonical_bytes`.
- `encoded_size(&Value) -> usize`: `canonical_bytes(v).len()`.
- `canonical_bytes(&Value) -> Vec<u8>`: serialize with **sorted keys**, compact
  separators (`,`/`:`, no spaces), `ensure_ascii = false` (UTF-8 passthrough).
  Implemented via a small canonicalizing serializer (sort map keys recursively)
  since `serde_json` does not sort by default. This exact form is parity-load-bearing.
  Python's `_json_bytes` also passes `default=str`, but that fallback only fires
  for non-JSON Python objects that have no `serde_json::Value` representation;
  over a parsed `&Value` every node is natively serializable, so `canonical_bytes`
  reproduces Python byte-for-byte without needing the fallback.

`mem-zero-copy`: redaction borrows the input `&Value`; only the digest's owned
`String` and `canonical_bytes`'s `Vec<u8>` allocate.

### Plugin audit family (`plugin.rs`) — kind is a PAYLOAD value (GC-audit-06)

Event types are a **fixed generic family**, NOT keyed by kind (no
`plugin.<kind>.*`): `PLUGIN_TOOL_INVOKED = "plugin.tool_invoked"`,
`PLUGIN_TOOL_COMPLETED = "plugin.tool_completed"`, `PLUGIN_ERROR = "plugin.error"`.
`plugin_kind` is a field on the payload.

`PluginSection` payload shape (from `audit_schema.py`), serialized nested under
`"plugin"`, `Default` + `skip_serializing_if`, but `plugin_id`/`plugin_kind`
**always serialized** (Python's `required=("plugin_id","plugin_kind")`):

| Field | Rust type | notes |
|---|---|---|
| `plugin_id` | `String` | always emitted |
| `plugin_kind` | `String` | always emitted; defaults to `"custom"` when manifest omits it |
| `plugin_version` | `Option<String>` | skip-if-none |
| `plugin_tool_name` | `Option<String>` | skip-if-none |
| `request_bytes` | `Option<u64>` | skip-if-none |
| `response_bytes` | `Option<u64>` | skip-if-none |
| `duration_ms` | `Option<f64>` | skip-if-none |
| `status` | `Option<String>` | `"ok"`/`"error"`; skip-if-none |
| `error_kind` | `Option<String>` | skip-if-none |
| `message_hash` | `Option<String>` | skip-if-none |
| `workspace_handle_id` | `Option<String>` | skip-if-none |
| `agent_id` | `Option<String>` | skip-if-none |
| `peak_resident_bytes` | `Option<u64>` | skip-if-none |

`eos-plugin-catalog` owns *wrapping* a tool's execute and supplying the
`PluginSection` (duration, status, `error_kind = <type name>`); `eos-audit` owns
the section shape + `build_plugin_event`-equivalent constructor:

```rust
#[must_use]
pub fn plugin_event(
    event_type: &str, // one of PLUGIN_TOOL_INVOKED / PLUGIN_TOOL_COMPLETED / PLUGIN_ERROR
    section: &PluginSection,
    node: AuditNode,
    clock: &dyn Clock,
) -> AuditEvent;
```

`source = AuditSource::Sandbox` is a **deliberate design decision, not Python
parity**: the daemon `build_plugin_event` dict carries no `source` field and no
`node` at all (`{"type", "payload": {"plugin": …}}`), so the four-literal
`AuditSource` concept does not exist on the Python plugin row. Sandbox is chosen
because plugin tools execute inside the sandbox; the enum has no `Plugin`
variant and we do **not** add one (GC-audit-06). Only the inner
`payload["plugin"]` object is byte-compatible with `build_plugin_event`'s
`PluginSection.as_dict()` section; the surrounding `AuditEvent` envelope
(`source`, `node`, `schema_version`, single `ts`) is **net-new**. The
constructor builds `payload = {"plugin": <PluginSection serialized>}` and sets
the `node` envelope. `PluginSection` fields with no `AuditNode` home
(`agent_id`, `workspace_handle_id`) stay inside `payload["plugin"]` — they are
**not** promoted into the node, matching the daemon `build_plugin_event` dict
shape, which carried no node of its own.

### `AuditError` (`error.rs`)

```rust
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuditError {
    #[error("audit jsonl write failed")]
    Jsonl(#[from] std::io::Error),
    #[error("audit event serialization failed")]
    Serialize(#[from] serde_json::Error),
    #[error("audit sink queue is full")]
    Backpressure,
}
```

`err-thiserror-lib`, `err-from-impl`, `err-lowercase-msg`. No `Box<dyn Error>`.

## 7. Concurrency & State Ownership

Per anchor §7. This crate is **synchronous** and runtime-agnostic — it takes
`&self`, never spawns a runtime, holds no `tokio` types (GC-audit-03).

- **Shared immutable handles:** sinks are stored and passed as
  `Arc<dyn AuditSink>` (`own-arc-shared`); cloning is a refcount bump.
- **`AuditEventBus` interior state:** `sinks: Vec<Arc<dyn AuditSink>>` (fixed at
  construction; YAGNI on dynamic subscribe — registered at the composition root)
  and `errors: std::sync::Mutex<Vec<AuditDispatchError>>`. **std::sync::Mutex**,
  not tokio — `publish` is sync and never `.await`s, so
  `async-no-lock-await`/`anti-lock-across-await` are trivially satisfied (state
  this explicitly: there is no await in this crate). The guard is held only to
  push one `AuditDispatchError` and is dropped before the next sink call
  (`async-clone-before-await` analog: minimize critical section).
- **`JsonlSink`:** owns a `PathBuf`; each `publish` does create-dir + open
  (`O_CREAT|O_APPEND`) + single `write` + close — append-mode write is the
  atomicity story (mirrors Python `os.open(...O_APPEND)`). Use it in tests and
  low-volume compatibility paths.
- **`BufferedJsonlSink`:** production file-backed sink. It owns a bounded
  `std::sync::mpsc::SyncSender<AuditEvent>` and a writer thread that owns the
  append-mode file handle. `publish` never performs disk IO on the caller's Tokio
  worker; it sends or returns `AuditError::Backpressure`. Shutdown flushes and
  joins the writer from the composition root.
- **No async channels, no `JoinSet`** — none apply to a synchronous fanout bus.
  The bridge is a dedicated sync writer thread so the `AuditSink` trait remains
  object-safe and runtime-agnostic.
- **CPU work** (hashing/canonical-JSON for redaction) runs inline on the caller's
  thread; volumes are small (single tool I/O blobs), so no `spawn_blocking`
  (`anti-premature-optimize`).

## 8. Behavior & Invariants

Preserved semantics (cite Python source):

1. **Error isolation (the crux, GC-audit-04).** `audit/bus.py` catches
   `BaseException` per subscriber so audit collection cannot interrupt the
   emitting domain path; failures land in `self.errors`. Rust translation:
   `AuditSink::publish` returns `Result`; `AuditEventBus::publish(&self, &event)`
   iterates **all** sinks, and for each `Err` pushes
   `AuditDispatchError { event: event.clone(), error }` into `errors`. The bus's
   own `publish` is infallible to its caller (signature returns `()`), so a bad
   sink never breaks emission. `catch_unwind` is **not** used (wrong tool: breaks
   under `panic=abort`, panic strategy undecided per anchor §14; sinks contract to
   not panic). Test harnesses inspect `errors` and fail scenarios explicitly.
2. **Producer-populates-known-IDs invariant.** `AuditNode` defaults all IDs to
   `None`; the collector never back-fills from payload text. Carried verbatim.
3. **Single `ts` source (GC-audit-01).** Python double-stamps (`AuditEvent`
   factory + `append_jsonl_event` spread, event-key wins). Collapse to one:
   `ts` set once from the injected `Clock` at `AuditEvent::new`; the JSONL writer
   serializes it as-is and adds nothing. A fixed test `Clock` ⇒ deterministic `ts`
   ⇒ exact golden bytes.
4. **Append-only, untruncated JSONL.** One JSON object + `\n` per event; parent
   dirs created; never truncates/rewrites. `schema_version` is a **top-level**
   field of the serialized row (`u32 = 1`) and is asserted in the golden.
5. **Canonical redaction is deterministic (GC-audit-02).** Same input ⇒ identical
   `*_shape`, `*_digest` (`sha256:` + sorted-key compact JSON), `*_bytes`; lists
   truncate to 5; completed-row `metadata` remaps inner `timings` →
   `domain_timings` while the row also carries a separate empty `timings: {}`.
6. **Plugin event-name genericity (GC-audit-06).** Exactly three plugin event
   types; `plugin_kind` is always a payload value, never encoded in the type
   string. A test asserts no emitted type matches `plugin.<kind>.*`.

Subtle risks flagged: (a) the engine_stream cycle (resolved by neutral
constructors); (b) JSON key-ordering — Rust `serde_json` does not sort by default,
so `canonical_bytes` MUST sort recursively or digests diverge from Python;
(c) scalar type-name strings must match a pinned mapping (golden-locked), since
Rust type names differ from Python's `type().__name__`.

## 9. SOLID & Principles Applied

- **DIP** (anchor §6 `AuditSink + AuditEventBus`): high-level emitters depend on
  the `AuditSink` trait; concrete `JsonlSink`/in-memory test sink/`NoopAuditSink`
  are injected at the composition root (`eos-runtime`). `eos-audit` depends only
  on the `eos-types` abstractions (`Clock`, IDs).
- **OCP:** new audit destinations = new `AuditSink` impls registered with the bus;
  no edit to bus dispatch. Event/source/section types are `#[non_exhaustive]`
  (`api-non-exhaustive`) for additive growth.
- **ISP:** `AuditSink` is a one-method write-only trait — no read/query surface
  forced on producers.
- **LSP:** every sink (real/noop/test) is substitutable behind `Arc<dyn AuditSink>`;
  the bus treats them uniformly.
- **SRP:** the crate only *describes and ships* audit data; it does not decide
  *when* events fire (that is producer/engine policy) and does not buffer/route.
- **KISS/YAGNI/DRY:** dropped dynamic subscribe/unsubscribe handle, generic
  `Mapping` JSONL arg, and the non-tool/non-plugin daemon sections (no agent-core
  caller). Single canonical-JSON helper reused by digest + byte-size + JSONL.
- **Non-goals respected:** no async/daemon ring (background work is an engine
  concern); no dependency on downstream crates; deep-sandbox audit buffer kept out.

## 10. Gap Closeouts (tracked requirements)

- **GC-audit-01 — single deterministic `ts`.** *(Derived requirement — not in
  the plan §13 Gap-closeout list; originates from the `jsonl.py`
  `append_jsonl_event` double-stamp invariant.)* Collapse Python's double `ts`
  stamping to one `UtcDateTime` set from an injected `Clock` (eos-types) at
  `AuditEvent::new`; JSONL writer serializes it unchanged. Proven by AC-audit-05.
- **GC-audit-02 — redaction deterministic & testable.** Pure functions over
  `&Value` producing stable `shape`/`redacted_shape`/`digest`(`sha256:`+sorted-key
  compact JSON)/`encoded_size`; lists truncate to 5. Proven by AC-audit-03/04.
- **GC-audit-03 — synchronous bus, typed IDs kept.** Bus stays sync (no tokio/
  async_trait); IDs are eos-types newtypes (except label `agent_name`/downstream
  `tool_name` as `String`). Proven by AC-audit-01/02 + compile.
- **GC-audit-04 — sink error isolation.** *(Derived requirement — not in the
  plan §13 Gap-closeout list; originates from `bus.py`'s `BaseException` capture
  into `self.errors`.)* A sink that *reports* a recoverable
  failure (returns `Err(AuditError)`) cannot break emission; the bus stashes
  `AuditDispatchError`s and its `publish` is infallible to callers. Unlike
  Python's `BaseException` capture, panics are **out of the isolation contract**
  (sink-no-panic invariant), pending the workspace panic-strategy decision
  (anchor §14) — `catch_unwind` is not used. Proven by AC-audit-06.
- **GC-audit-05 — no engine cycle.** *(Derived requirement — not in the plan §13
  Gap-closeout list; originates from anchor §5's DAG ban on `eos-audit` depending
  on downstream crates like `eos-engine`.)* `engine_stream.rs` exposes neutral
  constructors over borrowed `eos-audit`/`eos-types` inputs; it does NOT import
  `eos-engine` stream types; dispatch + node-ID precedence live in `eos-engine`.
  Proven by AC-audit-07 (crate compiles depending only on `eos-types`).
- **GC-audit-06 — plugin kind is payload, not name.** Three fixed `plugin.*`
  event types; `plugin_kind` only on the payload; no `plugin.<kind>.*` keys.
  Proven by AC-audit-08. The daemon-ring transport (`loader.py`'s
  `safe_emit(..., lane="normal")`) is **dropped**: plugin rows now ride the
  `AuditSink`/`AuditEventBus`/JSONL path like every other event, wrapped in a
  full `AuditEvent` (`source = AuditSource::Sandbox`, `node`) rather than the
  bare `build_plugin_event` dict.

## 11. Acceptance Criteria

TDD — write each failing test first, confirm it fails for the right reason, then
implement. Maps to anchor §11 "eos-audit: JSONL golden + deterministic redaction".

- **AC-audit-01** `AuditNode`/`AuditEvent` round-trip serde with skip-if-none and
  the exact Python JSON keys (`type`, `node`, nested `payload`).
  *Test:* `event::tests::node_event_serde_roundtrip`.
- **AC-audit-02** `AuditSource` serializes to `workflow|engine|sandbox|live_e2e`.
  *Test:* `event::tests::source_serde_strings`.
- **AC-audit-03** `shape`/`redacted_shape` match fixtures: dict keys preserved
  with type-name/`"<redacted>"` values; lists truncate to 5.
  *Test:* `redaction::tests::shape_and_redacted_match_fixtures`.
- **AC-audit-04** `digest` == `sha256:` + sorted-key compact-JSON hash and is
  stable across runs / key-insertion order; `encoded_size` matches byte length.
  *Test:* `redaction::tests::digest_is_canonical_and_deterministic` (property test
  over shuffled-key objects via `proptest`).
- **AC-audit-05 (golden)** A built tool-started + tool-completed `AuditEvent`
  with a fixed test `Clock` serializes to byte-exact golden JSONL lines including
  `schema_version: 1`, single `ts`, `timings: {}`, and `metadata.domain_timings`
  remap. The golden is the **authored Rust target shape**, not a captured Python
  JSONL line: `schema_version` and the collapsed single `ts` (GC-audit-01)
  intentionally differ from Python (which double-stamps `ts` via
  `append_jsonl_event`), so this is not a byte-for-byte Python replay.
  *Test:* `engine_stream::tests::tool_rows_match_golden_jsonl`.
- **AC-audit-06** `AuditEventBus::publish` with one failing + one ok sink delivers
  to the ok sink, records exactly one `AuditDispatchError`, and returns without
  error. *Test:* `bus::tests::failing_sink_is_isolated`.
- **AC-audit-07** The crate's dependency set is exactly `{eos-types}` + the listed
  externals (no downstream crate). *Test:* `tests/no_downstream_deps.rs` parsing
  `Cargo.toml` / a `cargo tree` assertion in CI.
- **AC-audit-08** No constructed plugin event `type` contains the kind; the three
  types are exactly the fixed strings and `plugin_kind` appears only in payload
  (`"custom"` fallback when absent). *Test:* `plugin::tests::kind_is_payload_only`.
- **AC-audit-09** `JsonlSink` appends untruncated lines, creates parent dirs, and
  preserves prior content across writes. *Test:* `jsonl::tests::append_only_untruncated`
  (tempdir RAII fixture, `test-fixture-raii`).

## 12. Implementation Checklist

1. `error.rs`: `AuditError` enum → compile. *(verify: `cargo check`)*
2. `node.rs`: `AuditNode` + `Default` + builder; serde skip-if-none → AC-audit-01.
3. `event.rs`: `AuditSource` enum, `SCHEMA_VERSION`, `AuditEvent` + `new(clock)` →
   AC-audit-01/02/05(ts).
4. `redaction.rs`: `canonical_bytes` (sorted-key), `digest`, `encoded_size`,
   `shape`, `redacted_shape` → AC-audit-03/04 (write proptest first).
5. `sink.rs`: `AuditSink` trait + `NoopAuditSink`.
6. `bus.rs`: `AuditEventBus` + `AuditDispatchError`; error isolation → AC-audit-06.
7. `jsonl.rs`: `JsonlSink` append writer → AC-audit-09.
8. `engine_stream.rs`: type consts + `tool_started`/`tool_completed` neutral
   constructors → AC-audit-05 golden, AC-audit-07 (no engine dep).
9. `plugin.rs`: `PluginSection` + 3 event constructors → AC-audit-08.
10. `lib.rs`: `pub use` re-exports + `//!` docs; wire workspace lints.
11. CI guard: `tests/no_downstream_deps.rs` → AC-audit-07.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-audit` per spec-conventions.md §13. Do not edit other crates' rows.
