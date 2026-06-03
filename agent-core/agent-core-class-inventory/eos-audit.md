# Crate `eos-audit` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-audit/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**14 types across 7 files.**

The `eos-audit` crate owns the write-only audit side channel. It defines the
structured event envelope — `AuditEvent` plus its correlation `AuditNode` (built
fluently via `AuditNodeBuilder`) and the emitting-package tag `AuditSource` — the
`AuditSink` write-only seam (with the disabled-collection `NoopAuditSink`), and
the synchronous in-process `AuditEventBus` that fans an event out to a fixed set
of sinks while isolating per-sink failures as `AuditDispatchError`. Persistence
is provided by the append-only `JsonlSink` and the production `BufferedJsonlSink`
(a bounded channel feeding a dedicated writer thread, drained by the
`BufferedAuditShutdown` guard); recoverable failures surface through the single
`AuditError` enum. Neutral constructors (`tool_started`/`tool_completed` in
`engine_stream.rs`, `plugin_event` + `PluginSection` in `plugin.rs`) build
engine- and plugin-sourced rows, and deterministic redaction helpers
(`digest`/`encoded_size`/`shape` in `redaction.rs`) summarize payloads without
leaking content. The crate depends only on `eos-types` (and does not own
lifecycle policy or import any downstream stream types); it is consumed by
`eos-engine`, `eos-plugin-catalog`, and the `eos-runtime` composition root.

## Contents

- **`eos-audit/src/bus.rs`** — `AuditDispatchError`, `AuditEventBus`
- **`eos-audit/src/error.rs`** — `AuditError`
- **`eos-audit/src/event.rs`** — `AuditSource`, `AuditEvent`
- **`eos-audit/src/jsonl.rs`** — `JsonlSink`, `WriterMsg`, `BufferedJsonlSink`, `BufferedAuditShutdown`
- **`eos-audit/src/node.rs`** — `AuditNode`, `AuditNodeBuilder`
- **`eos-audit/src/plugin.rs`** — `PluginSection`
- **`eos-audit/src/sink.rs`** — `AuditSink`, `NoopAuditSink`

---

## `eos-audit/src/bus.rs`

#### `AuditDispatchError`  ·  _struct_  ·  derives: `Debug`  ·  #[non_exhaustive]  ·  [L20]

A sink failure captured during fanout (the event plus the reported error).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `event` | `AuditEvent` | `pub` |
| `error` | `AuditError` | `pub` |

#### `AuditEventBus`  ·  _struct_  ·  [L34]

Single-process synchronous fanout bus over a fixed set of sinks, recording each sink's reported failure instead of propagating it.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `sinks` | `Vec<Arc<dyn AuditSink>>` |  |
| `errors` | `Mutex<Vec<AuditDispatchError>>` |  |

**Trait impls**: `Debug`

<details><summary>Methods (5)</summary>

`new`, `publish`, `error_count`, `take_errors`, `lock_errors`

</details>

---

## `eos-audit/src/error.rs`

#### `AuditError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L16]

The recoverable failures an audit sink can report (JSONL write IO, serialization, bounded-queue backpressure).

**Variants**:
- `Jsonl(#[from] std::io::Error)` — appending the event to a JSONL file failed.
- `Serialize(#[from] serde_json::Error)` — encoding the event to canonical JSON failed.
- `Backpressure` — the bounded sink queue is full; the event was dropped rather than block the caller.

---

## `eos-audit/src/event.rs`

#### `AuditSource`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L24]

The behavior-owning package that emitted an event (serializes to the Python `Literal` strings).

**Variants**: `Workflow`, `Engine`, `Sandbox`, `LiveE2e` (`#[serde(rename = "live_e2e")]`)

#### `AuditEvent`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L43]

A structured audit event emitted by a behavior-owning package; construct via `new` so `ts` is stamped once from the injected clock.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `schema_version` | `u32` | `pub` |
| `source` | `AuditSource` | `pub` |
| `event_type` | `String` | `pub`, `#[serde(rename = "type")]` |
| `node` | `AuditNode` | `pub` |
| `payload` | `JsonObject` | `pub`, `#[serde(default)]` |
| `correlation_id` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `ts` | `UtcDateTime` | `pub` |

<details><summary>Methods (1)</summary>

`new`

</details>

---

## `eos-audit/src/jsonl.rs`

#### `JsonlSink`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L40]

Open-append-close JSONL sink: one canonical JSON object per line; never truncates or rewrites.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `path` | `PathBuf` |  |

**Trait impls**: `AuditSink`

<details><summary>Methods (1)</summary>

`new`

</details>

#### `WriterMsg`  ·  _enum_  ·  private  ·  [L62]

Control message to the buffered writer thread.

**Variants**: `Event(Box<AuditEvent>)`, `Shutdown`

#### `BufferedJsonlSink`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L72]

Production file-backed sink: `publish` enqueues onto a bounded channel and a dedicated thread writes; a full queue returns `AuditError::Backpressure`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tx` | `SyncSender<WriterMsg>` |  |

**Trait impls**: `AuditSink`

<details><summary>Methods (1)</summary>

`new`

</details>

#### `BufferedAuditShutdown`  ·  _struct_  ·  derives: `Debug`  ·  [L142]

Shutdown guard for a `BufferedJsonlSink`'s writer thread: on `shutdown` or `Drop` it flushes and joins the writer.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `ctrl_tx` | `Option<SyncSender<WriterMsg>>` |  |
| `handle` | `Option<JoinHandle<()>>` |  |

**Trait impls**: `Drop`

<details><summary>Methods (2)</summary>

`shutdown`, `stop`

</details>

---

## `eos-audit/src/node.rs`

#### `AuditNode`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L23]

Correlation envelope carried by every audit event; producers populate only the ids they already know and missing ids are omitted from the wire form.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `request_id` | `Option<RequestId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `workflow_id` | `Option<WorkflowId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `iteration_id` | `Option<IterationId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `attempt_id` | `Option<AttemptId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `task_id` | `Option<TaskId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `agent_name` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `agent_run_id` | `Option<AgentRunId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `sandbox_id` | `Option<SandboxId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `tool_name` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |
| `tool_use_id` | `Option<ToolUseId>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none")]` |

<details><summary>Methods (1)</summary>

`builder`

</details>

#### `AuditNodeBuilder`  ·  _struct_  ·  derives: `Debug, Clone, Default`  ·  #[must_use]  ·  [L66]

Fluent builder for `AuditNode`; set only the ids a producer knows.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `node` | `AuditNode` |  |

<details><summary>Methods (11)</summary>

`request_id`, `workflow_id`, `iteration_id`, `attempt_id`, `task_id`, `agent_name`, `agent_run_id`, `sandbox_id`, `tool_name`, `tool_use_id`, `build`

</details>

---

## `eos-audit/src/plugin.rs`

#### `PluginSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L42]

Payload section for `plugin.*` events, serialized nested under `"plugin"`; `plugin_id`/`plugin_kind` are always emitted and the rest omitted when `None`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `plugin_id` | `String` | `pub` |
| `plugin_kind` | `String` | `pub` |
| `plugin_version` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `plugin_tool_name` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `request_bytes` | `Option<u64>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `response_bytes` | `Option<u64>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `duration_ms` | `Option<f64>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `status` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `error_kind` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `message_hash` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `workspace_handle_id` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `agent_id` | `Option<String>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `peak_resident_bytes` | `Option<u64>` | `pub`, `#[serde(skip_serializing_if = "Option::is_none", default)]` |

**Trait impls**: `Default`

---

## `eos-audit/src/sink.rs`

#### `AuditSink`  ·  _trait_  ·  bases: `Send + Sync`  ·  [L17]

Write-only audit side channel; implementations must not panic and report recoverable failures through `AuditError`.

**Trait items**:
- `fn publish(&self, event: &AuditEvent) -> Result<(), AuditError>;`

#### `NoopAuditSink`  ·  _struct_  ·  derives: `Debug, Clone, Copy, Default`  ·  [L28]

Audit sink used when collection is disabled; every publish is a no-op.

_Unit struct — no fields._

**Trait impls**: `AuditSink`
