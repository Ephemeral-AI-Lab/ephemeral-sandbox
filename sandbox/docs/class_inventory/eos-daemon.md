# Crate `eos-daemon` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-daemon/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**55 items (52 structs, 2 enums, 0 traits, 1 type alias) across 11 files.**

`eos-daemon` is the `eosd` tokio control-plane crate: it runs the newline-delimited compact-JSON protocol-v1 RPC server on an `AF_UNIX` socket plus a loopback-TCP listener, routes ops through the `OpTable` dispatcher, and owns the daemon-side ports that the severed Python `backend/src/sandbox` left impure. Its main item groups are the RPC server + config (`DaemonServer`, `ServerConfig`), the op dispatcher and its per-root OCC service cache / LayerStack commit + route providers (`OpTable`, `DispatchContext`, `OccServiceCache`, `LayerStackCommitTransaction`, `LayerStackRouteProvider`), the in-flight invocation registry with its TTL reaper (`InFlightRegistry`, `InFlightInvocation`, `ActiveCallGuard`), the audit ring buffer (`AuditBuffer`, `BufferedEvent`, `LaneCounters`), the command-session runtime (`CommandSession*`, the two finalizers), the daemon-local isolated-workspace lifecycle and its inverted snapshot/namespace ports (`DaemonLayerStackPort`, `DaemonNamespaceRuntime`, `CommandHandle`), and the plugin subsystem — PPC transport, process specs, OCC callbacks, and route state (`PpcClient`, `PluginProcessSpec`, `DaemonPluginState`, `PluginOperationRoute`, `CallbackLayerChange`).

## Contents

- **`eos-daemon/src/audit_buffer.rs`** — `BufferedEvent`, `LaneCounters`, `AuditBuffer`, `RingState`
- **`eos-daemon/src/command.rs`** — `CommandSessionOutput`, `CommandSessionOutputChunk`, `CommandSessionOutputCursor`, `CommandSession`, `CommandSessionRegistry`, `CompletedCommandSession`, `CommandWorkspace`, `IsolatedCommandWorkspace`, `CommandSessionStartSpec`, `CommandSessionFinalizer`, `IsolatedCommandSessionFinalizer`
- **`eos-daemon/src/dispatcher.rs`** — `DispatchContext`, `OpTable`, `PluginOverlayCommand`, `LayerStackCommitTransaction`, `PluginOverlayRunOutcome`, `OccRouteMetrics`, `TreeResourceStats`, `RunDirCleanup`, `PublishedCommitTimings`, `LayerStackRouteProvider`, `OccServiceLookup`, `OccServiceCacheStats`, `OccServiceCache`
- **`eos-daemon/src/error.rs`** — `DaemonError`, `Result`
- **`eos-daemon/src/invocation_registry.rs`** — `InFlightInvocation`, `InFlightRegistry`, `RegistryState`, `ActiveCallGuard`
- **`eos-daemon/src/isolated.rs`** — `CommandHandle`, `DaemonIsolatedState`, `DaemonLayerStackPort`, `DaemonNamespaceRuntime`
- **`eos-daemon/src/plugin/mod.rs`** — `LoadedPluginRuntime`, `PluginServiceSnapshot`, `PluginOperationRoute`, `DaemonPluginState`, `ParsedEnsure`, `StartedPluginService`, `ServiceHealthProbeTarget`
- **`eos-daemon/src/plugin/occ_callbacks.rs`** — `ApplyChangesetRequest`, `BaseHashPayload`, `CallbackLayerChange`
- **`eos-daemon/src/plugin/ppc_router.rs`** — `PendingRequest`, `PpcClient`
- **`eos-daemon/src/plugin/process.rs`** — `PluginServiceOverlay`, `PluginProcessSpec`, `PluginServiceProcess`
- **`eos-daemon/src/server.rs`** — `ServerConfig`, `DaemonServer`

---

## `eos-daemon/src/audit_buffer.rs`

#### `BufferedEvent`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L41]

A single buffered event: its monotonic sequence, lane, encoded size, and the payload (already stamped with `seq`/`lane`).

**Fields**

| name | type | vis |
|------|------|-----|
| `seq` | `u64` | `pub` |
| `lane` | `Lane` | `pub` |
| `encoded_bytes` | `u64` | `pub` |
| `payload` | `Value` | `pub` |

#### `LaneCounters`  ·  _struct_  ·  derives: `Debug, Clone, Copy, Default, PartialEq, Eq`  ·  [L55]

Per-lane retained-event/byte/dropped counters.

**Fields**

| name | type | vis |
|------|------|-----|
| `events` | `u64` | `pub` |
| `bytes` | `u64` | `pub` |
| `dropped` | `u64` | `pub` |

#### `AuditBuffer`  ·  _struct_  ·  [L71]

Bounded in-memory audit ring with lane-priority eviction (caps on event count AND byte size, edge-triggered pressure detection).

**Fields**

| name | type | vis |
|------|------|-----|
| `inner` | `Mutex<RingState>` |  |
| `boot_epoch_id` | `i64` |  |
| `pressure_threshold` | `f64` |  |

<details><summary>Methods (8)</summary>

`new`, `with_caps`, `boot_epoch_id`, `lock_state`, `append`, `pull`, `snapshot`, `default`

</details>

#### `RingState`  ·  _struct_  ·  derives: `Debug`  ·  [L79]

The mutex-guarded ring state. Held synchronously only; never across `.await`.

**Fields**

| name | type | vis |
|------|------|-----|
| `max_events` | `u64` |  |
| `max_bytes` | `u64` |  |
| `next_seq` | `u64` |  |
| `lost_before_seq` | `u64` |  |
| `dropped_total` | `u64` |  |
| `all` | `VecDeque<BufferedEvent>` |  |
| `lanes` | `[VecDeque<BufferedEvent>; 3]` |  |
| `counters` | `[LaneCounters; 3]` |  |
| `pressure_above` | `bool` |  |

---

## `eos-daemon/src/command.rs`

#### `CommandSessionOutput`  ·  _struct_  ·  [L293]

Bounded ring + spool bookkeeping for one command session's combined stdout/stderr stream (Linux only).

**Fields**

| name | type | vis |
|------|------|-----|
| `chunks` | `Mutex<VecDeque<CommandSessionOutputChunk>>` |  |
| `bytes` | `Mutex<usize>` |  |
| `next_byte_offset` | `Mutex<u64>` |  |
| `spool_bytes` | `Mutex<u64>` |  |
| `spool_truncated` | `Mutex<bool>` |  |

<details><summary>Methods (6)</summary>

`new`, `append`, `read_since`, `all_recent`, `note_spooled`, `spool_truncated`

</details>

#### `CommandSessionOutputChunk`  ·  _struct_  ·  [L302]

One contiguous output chunk with its byte-offset window into the session stream.

**Fields**

| name | type | vis |
|------|------|-----|
| `start` | `u64` |  |
| `end` | `u64` |  |
| `text` | `String` |  |

#### `CommandSessionOutputCursor`  ·  _struct_  ·  derives: `Clone, Copy, Default`  ·  [L310]

A per-consumer read cursor (model vs. notification) over the session output stream.

**Fields**

| name | type | vis |
|------|------|-----|
| `next_seq` | `u64` |  |
| `next_byte_offset` | `u64` |  |

#### `CommandSession`  ·  _struct_  ·  [L454]

A live command session: its pty writer, output ring, reader-done channel, and cancel/interrupt + per-consumer cursors.

**Fields**

| name | type | vis |
|------|------|-----|
| `id` | `String` |  |
| `agent_id` | `String` |  |
| `command` | `String` |  |
| `started_at` | `Instant` |  |
| `pgid` | `i32` |  |
| `writer` | `Mutex<File>` |  |
| `output` | `Arc<CommandSessionOutput>` |  |
| `reader_done` | `Mutex<Option<std_mpsc::Receiver<()>>>` |  |
| `cancelled` | `Mutex<bool>` |  |
| `interrupted` | `Mutex<bool>` |  |
| `model_cursor` | `Mutex<CommandSessionOutputCursor>` |  |
| `notification_cursor` | `Mutex<CommandSessionOutputCursor>` |  |

<details><summary>Methods (2)</summary>

`read_model_output`, `read_notification_output`

</details>

#### `CommandSessionRegistry`  ·  _struct_  ·  [L483]

Process-global registry of live + completed command sessions, keyed by session id.

**Fields**

| name | type | vis |
|------|------|-----|
| `sessions` | `Mutex<HashMap<String, Arc<CommandSession>>>` |  |
| `completed` | `Mutex<HashMap<String, CompletedCommandSession>>` |  |
| `counter` | `AtomicU64` |  |

<details><summary>Methods (9)</summary>

`new`, `next_id`, `insert`, `get`, `remove`, `count_by_agent`, `push_completed`, `take_completed_result`, `collect_completed`

</details>

#### `CompletedCommandSession`  ·  _struct_  ·  [L490]

A finished session's completion payload plus a notification-delivered latch.

**Fields**

| name | type | vis |
|------|------|-----|
| `completion` | `Value` |  |
| `notification_delivered` | `bool` |  |

#### `CommandWorkspace`  ·  _struct_  ·  [L611]

The shared-workspace overlay context a command session finalizes through (snapshot lease + upperdir + run dir).

**Fields**

| name | type | vis |
|------|------|-----|
| `root` | `PathBuf` |  |
| `lease_id` | `String` |  |
| `manifest` | `eos_layerstack::Manifest` |  |
| `manifest_version` | `i64` |  |
| `upperdir` | `PathBuf` |  |
| `run_dir` | `PathBuf` |  |
| `output_path` | `PathBuf` |  |
| `final_path` | `PathBuf` |  |

#### `IsolatedCommandWorkspace`  ·  _struct_  ·  [L623]

The isolated-workspace command context (cloned `CommandHandle` plus result/final paths), audited but never OCC-published.

**Fields**

| name | type | vis |
|------|------|-----|
| `handle` | `crate::isolated::CommandHandle` |  |
| `output_path` | `PathBuf` |  |
| `final_path` | `PathBuf` |  |

#### `CommandSessionStartSpec`  ·  _struct_  ·  [L630]

The start-time identity + timeout for a command session, shared by the shared and isolated launch paths.

**Fields**

| name | type | vis |
|------|------|-----|
| `id` | `String` |  |
| `invocation_id` | `String` |  |
| `agent_id` | `String` |  |
| `command` | `String` |  |
| `timeout_seconds` | `Option<f64>` |  |

#### `CommandSessionFinalizer`  ·  _struct_  ·  [L1020]

Owns wait + capture + OCC-publish + cleanup for a shared-workspace command session.

**Fields**

| name | type | vis |
|------|------|-----|
| `session` | `Arc<CommandSession>` |  |
| `child` | `Child` |  |
| `workspace` | `CommandWorkspace` |  |

<details><summary>Methods (1)</summary>

`finish`

</details>

#### `IsolatedCommandSessionFinalizer`  ·  _struct_  ·  [L1109]

Owns wait + capture + audit-only finalize + cleanup for an isolated-workspace command session.

**Fields**

| name | type | vis |
|------|------|-----|
| `session` | `Arc<CommandSession>` |  |
| `child` | `Child` |  |
| `workspace` | `IsolatedCommandWorkspace` |  |

<details><summary>Methods (1)</summary>

`finish`

</details>

---

## `eos-daemon/src/dispatcher.rs`

#### `DispatchContext`  ·  _struct_  ·  generics: `<'ctx>`  ·  derives: `Clone, Copy, Default`  ·  [L72]

Per-dispatch daemon services used by handlers that need runtime state.

**Fields**

| name | type | vis |
|------|------|-----|
| `invocation_registry` | `Option<&'ctx InFlightRegistry>` |  |

<details><summary>Methods (2)</summary>

`empty`, `with_invocation_registry`

</details>

#### `OpTable`  ·  _struct_  ·  derives: `Clone, Default`  ·  [L100]

The op routing table; re-registering the same handler is a no-op, a colliding handler under a claimed op is rejected.

**Fields**

| name | type | vis |
|------|------|-----|
| `handlers` | `HashMap<String, Handler>` |  |

<details><summary>Methods (5)</summary>

`with_builtins`, `register`, `register_builtin`, `dispatch`, `dispatch_with_context`

</details>

#### `PluginOverlayCommand`  ·  _struct_  ·  [L872]

The spec for one oneshot plugin-overlay ns-runner command (command + env + workspace targeting).

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_stack_root` | `PathBuf` | `pub(crate)` |
| `invocation_id` | `String` | `pub(crate)` |
| `agent_id` | `String` | `pub(crate)` |
| `public_op` | `String` | `pub(crate)` |
| `plugin_id` | `String` | `pub(crate)` |
| `op_name` | `String` | `pub(crate)` |
| `command` | `Vec<String>` | `pub(crate)` |
| `env` | `BTreeMap<String, String>` | `pub(crate)` |
| `timeout_seconds` | `Option<f64>` | `pub(crate)` |

#### `LayerStackCommitTransaction`  ·  _struct_  ·  derives: `Clone`  ·  [L1347]

The per-root `CommitTransactionPort` impl: revalidates a prepared changeset against the live manifest and publishes through `LayerStack`.

**Fields**

| name | type | vis |
|------|------|-----|
| `root` | `PathBuf` |  |

<details><summary>Methods (1)</summary>

`revalidate_and_publish`

</details>

#### `PluginOverlayRunOutcome`  ·  _struct_  ·  [L1351]

The captured result of one plugin-overlay run: ns-runner result, OCC changeset, plugin result, and per-phase timing inputs.

**Fields**

| name | type | vis |
|------|------|-----|
| `runner` | `RunResult` |  |
| `changeset` | `ChangesetResult` |  |
| `plugin_result` | `Option<Value>` |  |
| `path_kinds` | `Vec<(String, String)>` |  |
| `route_metrics` | `OccRouteMetrics` |  |
| `route_s` | `f64` |  |
| `capture_s` | `f64` |  |
| `occ_s` | `f64` |  |
| `upperdir_stats` | `TreeResourceStats` |  |

#### `OccRouteMetrics`  ·  _struct_  ·  derives: `Clone, Copy, Debug, Default, PartialEq, Eq`  ·  [L1364]

Counts of gated vs. direct (gitignored) paths in a changeset, for OCC route timings.

**Fields**

| name | type | vis |
|------|------|-----|
| `gated_path_count` | `usize` |  |
| `direct_path_count` | `usize` |  |

#### `TreeResourceStats`  ·  _struct_  ·  derives: `Clone, Copy, Debug`  ·  [L1370]

Aggregate file/dir/byte/entry counts over a directory tree (bounded by an entry limit) for resource timings.

**Fields**

| name | type | vis |
|------|------|-----|
| `exists` | `f64` |  |
| `bytes` | `f64` |  |
| `file_count` | `f64` |  |
| `dir_count` | `f64` |  |
| `entry_count` | `f64` |  |
| `truncated` | `f64` |  |

<details><summary>Methods (2)</summary>

`missing`, `collect`

</details>

#### `RunDirCleanup`  ·  _struct_  ·  [L1443]

RAII guard that removes a transient overlay run dir on drop.

**Fields**

| name | type | vis |
|------|------|-----|
| `0` | `PathBuf` |  |

<details><summary>Methods (1)</summary>

`drop`

</details>

#### `PublishedCommitTimings`  ·  _struct_  ·  [L1718]

The validate/publish/maintenance timing inputs gathered when a commit publishes a new manifest.

**Fields**

| name | type | vis |
|------|------|-----|
| `validate_s` | `f64` |  |
| `publish_s` | `f64` |  |
| `maintenance_timings` | `BTreeMap<String, f64>` |  |
| `total_start` | `Instant` |  |

#### `LayerStackRouteProvider`  ·  _struct_  ·  derives: `Clone`  ·  [L1726]

The per-root `OccRouteProvider` impl: answers `.gitignore` routing and current base-hash queries from the live `LayerStack`.

**Fields**

| name | type | vis |
|------|------|-----|
| `root` | `PathBuf` |  |

<details><summary>Methods (2)</summary>

`is_ignored`, `base_hash`

</details>

#### `OccServiceLookup`  ·  _struct_  ·  [L1981]

The result of an OCC service-cache lookup: the shared service plus the lock-wait/hit/eviction telemetry to fold into timings.

**Fields**

| name | type | vis |
|------|------|-----|
| `service` | `Arc<OccService<LayerStackCommitTransaction>>` |  |
| `lock_wait_s` | `f64` |  |
| `cache_hit` | `bool` |  |
| `cache_created` | `bool` |  |
| `evicted_count` | `usize` |  |
| `cache_size` | `usize` |  |

<details><summary>Methods (1)</summary>

`insert_timings`

</details>

#### `OccServiceCacheStats`  ·  _struct_  ·  derives: `Default`  ·  [L2029]

Cumulative hit/miss/create/eviction + lock-wait counters for the OCC service cache.

**Fields**

| name | type | vis |
|------|------|-----|
| `hits_total` | `u64` |  |
| `misses_total` | `u64` |  |
| `creates_total` | `u64` |  |
| `evictions_total` | `u64` |  |
| `lock_wait_s_total` | `f64` |  |
| `lock_wait_s_max` | `f64` |  |

#### `OccServiceCache`  ·  _struct_  ·  derives: `Default`  ·  [L2039]

The process-global per-root OCC single-writer service cache (LRU-bounded), the live OCC write path's owner.

**Fields**

| name | type | vis |
|------|------|-----|
| `entries` | `HashMap<String, Arc<OccService<LayerStackCommitTransaction>>>` |  |
| `lru` | `VecDeque<String>` |  |
| `stats` | `OccServiceCacheStats` |  |

<details><summary>Methods (5)</summary>

`record_lock_wait`, `get`, `insert_or_get`, `touch`, `evict_oldest`

</details>

---

## `eos-daemon/src/error.rs`

#### `DaemonError`  ·  _enum_  ·  derives: `Debug, Error`  ·  `#[non_exhaustive]`  ·  [L16]

Failures surfaced by the daemon server, dispatcher, audit ring, and the injected port implementations.

**Variants**: `Protocol(eos_protocol::ProtocolError)`, `Io(std::io::Error)`, `UnknownOp(String)`, `InvalidEnvelope(String)`, `RequestTooLarge { limit: usize }`, `Unauthorized`, `Forbidden(String)`, `StateLockPoisoned(&'static str)`, `LayerStack(eos_layerstack::LayerStackError)`, `Occ(eos_occ::OccError)`, `OverlayPipeline(String)`, `Plugin(eos_plugin::PluginError)`, `Isolated(eos_isolated::IsolatedError)`

<details><summary>Methods (1)</summary>

`wire_kind`

</details>

#### `Result`  ·  _type alias_  ·  `= core::result::Result<T, DaemonError>`  ·  [L104]

Convenience alias for fallible daemon operations.

---

## `eos-daemon/src/invocation_registry.rs`

#### `InFlightInvocation`  ·  _struct_  ·  derives: `Debug`  ·  [L47]

One tracked daemon-side invocation (abort handle, owning agent, heartbeat, background flag, optional process group, TTL-reaped latch).

**Fields**

| name | type | vis |
|------|------|-----|
| `invocation_id` | `String` | `pub` |
| `abort` | `AbortHandle` | `pub` |
| `agent_id` | `String` | `pub` |
| `op` | `String` | `pub` |
| `last_seen` | `f64` | `pub` |
| `background` | `bool` | `pub` |
| `active_calls` | `u32` | `pub` |
| `process_group_id` | `Option<i32>` | `pub` |
| `ttl_reaped` | `bool` | `pub` |

#### `InFlightRegistry`  ·  _struct_  ·  derives: `Debug`  ·  [L74]

Tracks daemon-side tasks by invocation id for cancellation + TTL cleanup.

**Fields**

| name | type | vis |
|------|------|-----|
| `inner` | `Mutex<RegistryState>` |  |
| `ttl_s` | `f64` |  |
| `reaper_interval_s` | `f64` |  |

<details><summary>Methods (14)</summary>

`new`, `from_env`, `reaper_interval_s`, `lock_state`, `register`, `register_process_group`, `clear_process_group`, `deregister`, `cancel`, `heartbeat`, `count_by_agent`, `enter_call`, `ttl_sweep`, `metrics`

</details>

#### `RegistryState`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L81]

The mutex-guarded registry map + cumulative TTL-reaped counter.

**Fields**

| name | type | vis |
|------|------|-----|
| `by_invocation` | `HashMap<String, InFlightInvocation>` |  |
| `ttl_reaped_total` | `u64` |  |

#### `ActiveCallGuard`  ·  _struct_  ·  generics: `<'r>`  ·  derives: `Debug`  ·  [L264]

RAII guard counting one active runtime call against an invocation; decrements `active_calls` on drop.

**Fields**

| name | type | vis |
|------|------|-----|
| `registry` | `&'r InFlightRegistry` |  |
| `invocation_id` | `String` |  |

<details><summary>Methods (1)</summary>

`drop`

</details>

---

## `eos-daemon/src/isolated.rs`

#### `CommandHandle`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L54]

A cloned, daemon-local view of an isolated workspace handle (namespace FDs, overlay dirs, manifest pin) handed to the command-session dispatcher. (Linux only.)

**Fields**

| name | type | vis |
|------|------|-----|
| `agent_id` | `String` | `pub` |
| `workspace_handle_id` | `String` | `pub` |
| `layer_stack_root` | `PathBuf` | `pub` |
| `manifest_version` | `i64` | `pub` |
| `manifest_root_hash` | `String` | `pub` |
| `workspace_root` | `PathBuf` | `pub` |
| `scratch_dir` | `PathBuf` | `pub` |
| `upperdir` | `PathBuf` | `pub` |
| `workdir` | `PathBuf` | `pub` |
| `layer_paths` | `Vec<PathBuf>` | `pub` |
| `ns_fds` | `HashMap<String, i32>` | `pub` |
| `cgroup_path` | `Option<PathBuf>` | `pub` |

#### `DaemonIsolatedState`  ·  _struct_  ·  [L69]

The daemon-local isolated-workspace state: one `eos-isolated` session plus the active command-session-id -> agent map.

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_stack_root` | `PathBuf` (cfg `target_os = "linux"`) |  |
| `session` | `DaemonSession` |  |
| `active_command_sessions` | `HashMap<String, String>` |  |

#### `DaemonLayerStackPort`  ·  _struct_  ·  derives: `Clone`  ·  [L90]

The daemon's `LayerStackSnapshotPort` impl: acquires/releases snapshot leases against a shared `LayerStack`.

**Fields**

| name | type | vis |
|------|------|-----|
| `stack` | `Arc<Mutex<LayerStack>>` |  |

<details><summary>Methods (3)</summary>

`acquire_snapshot`, `release_lease`, `active_lease_count`

</details>

#### `DaemonNamespaceRuntime`  ·  _struct_  ·  derives: `Default`  ·  [L129]

The daemon's `NamespaceRuntimePort` impl: spawns the single-threaded `eosd ns-holder`/`ns-runner` children and wires their pinned namespace FDs in (the daemon never enters a namespace itself).

<details><summary>Methods (7)</summary>

`spawn_ns_holder`, `open_ns_fds`, `mount_overlay`, `configure_dns`, `signal_net_ready`, `create_cgroup`, `kill_holder`

</details>

---

## `eos-daemon/src/plugin/mod.rs`

#### `LoadedPluginRuntime`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L37]

The recorded state of one ensured plugin: digest, registered ops, operation routes, services, and process specs.

**Fields**

| name | type | vis |
|------|------|-----|
| `digest` | `String` |  |
| `registered_ops` | `Vec<String>` |  |
| `operation_routes` | `BTreeMap<String, PluginOperationRoute>` |  |
| `services` | `Vec<PluginServiceStatus>` |  |
| `service_processes` | `Vec<PluginProcessSpec>` |  |
| `runtime_loaded` | `bool` |  |

#### `PluginServiceSnapshot`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L47]

The snapshot lease + manifest key + overlay dirs pinned for a running plugin service process.

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_stack_root` | `String` |  |
| `lease_id` | `String` |  |
| `manifest_key` | `String` |  |
| `layer_paths` | `Vec<String>` |  |
| `overlay` | `Option<PluginServiceOverlay>` |  |

#### `PluginOperationRoute`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L56]

A resolved route for one public plugin op: intent, dispatch mode inputs, and any backing service identity/command.

**Fields**

| name | type | vis |
|------|------|-----|
| `plugin_id` | `String` |  |
| `op_name` | `String` |  |
| `public_op` | `String` |  |
| `layer_stack_root` | `Option<String>` |  |
| `intent` | `Intent` |  |
| `auto_workspace_overlay` | `bool` |  |
| `service_id` | `Option<String>` |  |
| `service_instance_id` | `Option<String>` |  |
| `service_key` | `Option<PluginServiceKey>` |  |
| `service_mode` | `Option<ServiceMode>` |  |
| `service_command` | `Vec<String>` |  |
| `service_ppc_protocol_version` | `Option<u32>` |  |
| `timeout_ms` | `Option<u64>` |  |

<details><summary>Methods (2)</summary>

`dispatch_mode`, `to_json`

</details>

#### `DaemonPluginState`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L101]

The process-global plugin registry: loaded plugins, connected PPC clients, running service processes, snapshots, and per-service refresh locks.

**Fields**

| name | type | vis |
|------|------|-----|
| `loaded` | `BTreeMap<String, LoadedPluginRuntime>` |  |
| `service_ppc_clients` | `BTreeMap<String, SharedPpcClient>` |  |
| `service_processes` | `BTreeMap<String, process::PluginServiceProcess>` |  |
| `service_snapshots` | `BTreeMap<String, PluginServiceSnapshot>` |  |
| `service_refresh_locks` | `BTreeMap<String, Arc<Mutex<()>>>` |  |

#### `ParsedEnsure`  ·  _struct_  ·  [L339]

The parsed `api.plugin.ensure` request (plugin id/digest plus the derived ops/routes/services/process specs).

**Fields**

| name | type | vis |
|------|------|-----|
| `plugin_id` | `String` |  |
| `plugin_digest` | `String` |  |
| `registered_ops` | `Vec<String>` |  |
| `operation_routes` | `BTreeMap<String, PluginOperationRoute>` |  |
| `services` | `Vec<PluginServiceStatus>` |  |
| `service_processes` | `Vec<PluginProcessSpec>` |  |
| `runtime_loaded` | `bool` |  |

<details><summary>Methods (2)</summary>

`from_args`, `from_manifest`

</details>

#### `StartedPluginService`  ·  _struct_  ·  [L640]

A just-spawned plugin service process bundled with its PPC client and snapshot, awaiting insertion into daemon state.

**Fields**

| name | type | vis |
|------|------|-----|
| `service_instance_id` | `String` |  |
| `process` | `process::PluginServiceProcess` |  |
| `client` | `SharedPpcClient` |  |
| `snapshot` | `PluginServiceSnapshot` |  |

#### `ServiceHealthProbeTarget`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L977]

A connected service to PPC health-probe: its identity, pinned manifest key, and shared PPC client.

**Fields**

| name | type | vis |
|------|------|-----|
| `plugin_id` | `String` |  |
| `service_id` | `String` |  |
| `service_instance_id` | `String` |  |
| `manifest_key` | `String` |  |
| `client` | `SharedPpcClient` |  |

---

## `eos-daemon/src/plugin/occ_callbacks.rs`

#### `ApplyChangesetRequest`  ·  _struct_  ·  derives: `Debug, Deserialize`  ·  [L100]

The deserialized `daemon.occ.apply_changeset` callback body (root + optional snapshot version + changes + base hashes).

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_stack_root` | `String` |  |
| `snapshot_version` | `Option<u64>` |  |
| `changes` | `Vec<CallbackLayerChange>` |  |
| `base_hashes` | `Vec<BaseHashPayload>` |  |

#### `BaseHashPayload`  ·  _struct_  ·  derives: `Debug, Deserialize`  ·  [L110]

A per-path base-hash entry in an OCC callback request.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `String` |  |
| `hash` | `Option<String>` |  |

#### `CallbackLayerChange`  ·  _enum_  ·  derives: `Debug, Deserialize`  ·  `#[serde(tag = "kind", rename_all = "snake_case")]`  ·  [L118]

The wire form of a single layer change in an OCC callback, mapped to `eos_protocol::LayerChange`.

**Variants**: `Write { path: String, content_utf8: Option<String>, content_bytes: Option<Vec<u8>> }`, `Delete { path: String }`, `Symlink { path: String, source_path: String }`, `OpaqueDir { path: String }`

<details><summary>Methods (1)</summary>

`into_layer_change`

</details>

---

## `eos-daemon/src/plugin/ppc_router.rs`

#### `PendingRequest`  ·  _struct_  ·  [L32]

One in-flight PPC request awaiting its reply: the reply channel and an optional callback handler for self-managed ops.

**Fields**

| name | type | vis |
|------|------|-----|
| `reply_tx` | `mpsc::Sender<PpcResult>` |  |
| `callback_handler` | `Option<CallbackHandler>` |  |

#### `PpcClient`  ·  _struct_  ·  [L37]

The daemon-side PPC transport over a plugin's `AF_UNIX` socket: a dedicated reader thread routes reply frames by `message_id` and services plugin-originated callbacks concurrently.

**Fields**

| name | type | vis |
|------|------|-----|
| `writer` | `Arc<Mutex<UnixStream>>` |  |
| `pending` | `Arc<Mutex<HashMap<String, PendingRequest>>>` |  |

<details><summary>Methods (7)</summary>

`fmt`, `new`, `round_trip`, `round_trip_with_callbacks`, `send_request`, `write_frame`, `remove_pending`

</details>

---

## `eos-daemon/src/plugin/process.rs`

#### `PluginServiceOverlay`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L40]

The overlay run dir + layer paths + upper/work dirs for launching a plugin service inside a workspace overlay.

**Fields**

| name | type | vis |
|------|------|-----|
| `run_dir` | `PathBuf` | `pub(super)` |
| `layer_paths` | `Vec<PathBuf>` | `pub(super)` |
| `upperdir` | `PathBuf` | `pub(super)` |
| `workdir` | `PathBuf` | `pub(super)` |

#### `PluginProcessSpec`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L48]

The launch contract for one plugin service process: its key, argv, PPC protocol version, and stable `/eos/plugin/ppc/*.sock` endpoint.

**Fields**

| name | type | vis |
|------|------|-----|
| `key` | `PluginServiceKey` |  |
| `command` | `Vec<String>` |  |
| `ppc_protocol_version` | `u32` |  |
| `socket_path` | `PathBuf` |  |

<details><summary>Methods (11)</summary>

`new`, `new_with_socket_root`, `environment`, `service_instance_id`, `spawn`, `spawn_command`, `spawn_connected_with_overlay`, `spawn_for_overlay`, `spawn_overlay_runner`, `overlay_run_request`, `to_json`

</details>

#### `PluginServiceProcess`  ·  _struct_  ·  derives: `Debug`  ·  [L269]

A spawned plugin service child process with its process group + teardown latch.

**Fields**

| name | type | vis |
|------|------|-----|
| `spec` | `PluginProcessSpec` |  |
| `child` | `Child` |  |
| `process_group_id` | `Option<i32>` |  |
| `torn_down` | `bool` |  |

<details><summary>Methods (4)</summary>

`pid`, `status_json`, `teardown`, `drop`

</details>

---

## `eos-daemon/src/server.rs`

#### `ServerConfig`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L56]

Where the daemon binds + writes its pid, plus the optional loopback TCP listener and its auth token.

**Fields**

| name | type | vis |
|------|------|-----|
| `socket_path` | `PathBuf` | `pub` |
| `pid_path` | `PathBuf` | `pub` |
| `tcp_host` | `Option<String>` | `pub` |
| `tcp_port` | `Option<u16>` | `pub` |
| `auth_token` | `Option<String>` | `pub` |

#### `DaemonServer`  ·  _struct_  ·  [L76]

The running daemon: the op table, audit ring, invocation registry, and the shutdown token. Orchestrates but never enters a namespace.

**Fields**

| name | type | vis |
|------|------|-----|
| `config` | `ServerConfig` |  |
| `op_table` | `OpTable` |  |
| `audit` | `AuditBuffer` |  |
| `invocation_registry` | `Arc<InFlightRegistry>` |  |
| `shutdown` | `CancellationToken` |  |

<details><summary>Methods (7)</summary>

`new`, `shutdown_token`, `serve`, `handle_connection`, `dispatch_bytes`, `dispatch_request`, `strip_tcp_auth`

</details>
