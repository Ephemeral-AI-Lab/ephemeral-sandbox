# Crate `eos-protocol` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-protocol/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**40 items (32 structs, 8 enums, 0 traits, 0 type aliases) across 4 files.**

`eos-protocol` is the dependency-free source of truth for the `eosd` runtime's wire protocol and content-addressed-store byte identity. It owns the two correctness-bearing CAS hashes (`manifest_root_hash`, `layer_digest`) plus the framed newline-delimited-JSON envelope encode/decode, and ports the Python `backend/src/sandbox` schema for shared tool-verb request/response models, the daemon audit-event sections, and frozen protocol constants — all kept byte-identical or canonically-equal to the live Python.

## Contents

- **`eos-protocol/src/audit.rs`** — `Lane`, `DaemonSection`, `LayerStackSection`, `OverlayWorkspaceSection`, `IsolatedWorkspaceSection`, `OccSection`, `PluginSection`, `BackgroundToolSection`, `ToolCallSection`, `OsResourceSection`
- **`eos-protocol/src/cas.rs`** — `CasError`, `LayerPath`, `LayerRef`, `Manifest`, `LayerChange`
- **`eos-protocol/src/envelope.rs`** — `ProtocolError`, `Request`, `ErrorEnvelope`, `ErrorBody`, `ErrorKind`, `Envelope`
- **`eos-protocol/src/models.rs`** — `Intent`, `ConflictInfo`, `ReadFileArgs`, `WriteFileArgs`, `EditFileArgs`, `CommandOutput`, `ExecCommandArgs`, `ExecCommandResult`, `CommandSessionWriteArgs`, `CommandSessionCancelArgs`, `GlobArgs`, `GrepArgs`, `ReadFileResult`, `WriteFileResult`, `EditFileResult`, `GlobResult`, `GrepResult`, `SearchReplaceEdit`, `SearchReplaceError`

---

## `eos-protocol/src/audit.rs`

#### `Lane`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  `#[serde(rename_all = "snake_case")]`  ·  [L32]

Audit lane; storage order is `_LANES`, eviction tries `sample` first and `critical` last (`_EVICTION_ORDER`).

**Variants**: `Critical`, `Normal`, `Sample`

<details><summary>Methods (0)</summary>

(associated consts only: `STORAGE_ORDER`, `EVICTION_ORDER`)

</details>

#### `DaemonSection`  ·  _struct_  ·  derives: `Debug, Clone, Default, PartialEq, Serialize, Deserialize`  ·  [L50]

`daemon` audit section (PORT audit_schema.py:28-39).

**Fields**

| name | type | vis |
|------|------|-----|
| `boot_epoch_id` | `Option<i64>` | `pub` |
| `pid` | `Option<i64>` | `pub` |
| `pressure` | `Option<f64>` | `pub` |
| `retained_events` | `Option<i64>` | `pub` |
| `retained_bytes` | `Option<i64>` | `pub` |

#### `LayerStackSection`  ·  _struct_  ·  derives: `Debug, Clone, Default, PartialEq, Serialize, Deserialize`  ·  [L65]

`layer_stack` audit section (PORT audit_schema.py:42-63).

**Fields**

| name | type | vis |
|------|------|-----|
| `operation_id` | `Option<String>` | `pub` |
| `operation_step` | `Option<i64>` | `pub` |
| `lease_id` | `Option<String>` | `pub` |
| `owner_request_id` | `Option<String>` | `pub` |
| `manifest_version` | `Option<i64>` | `pub` |
| `manifest_root_hash` | `Option<String>` | `pub` |
| `layer_count` | `Option<i64>` | `pub` |
| `lease_wait_ms` | `Option<f64>` | `pub` |
| `lock_wait_ms` | `Option<f64>` | `pub` |
| `lease_hold_ms` | `Option<f64>` | `pub` |
| `prepare_snapshot_ms` | `Option<f64>` | `pub` |
| `squash_trigger_reason` | `Option<String>` | `pub` |
| `squash_input_layers` | `Option<i64>` | `pub` |
| `squash_result_layers` | `Option<i64>` | `pub` |
| `squash_failure_kind` | `Option<String>` | `pub` |

#### `OverlayWorkspaceSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  [L101]

`overlay_workspace` audit section; `workspace_mode` defaults `"ephemeral"` and is always emitted.

**Fields**

| name | type | vis |
|------|------|-----|
| `operation_id` | `Option<String>` | `pub` |
| `workspace_mode` | `String` | `pub` |
| `workspace_handle_id` | `Option<String>` | `pub` |
| `lease_id` | `Option<String>` | `pub` |
| `manifest_root_hash` | `Option<String>` | `pub` |
| `mount_ms` | `Option<f64>` | `pub` |
| `cleanup_ms` | `Option<f64>` | `pub` |
| `scratch_removed` | `Option<bool>` | `pub` |
| `cleanup_failure_kind` | `Option<String>` | `pub` |
| `committed_layer_id` | `Option<String>` | `pub` |
| `publish_layer_ms` | `Option<f64>` | `pub` |
| `changed_path_count` | `Option<i64>` | `pub` |
| `upperdir_bytes` | `Option<i64>` | `pub` |

<details><summary>Methods (1)</summary>

`default`

</details>

#### `IsolatedWorkspaceSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  [L153]

`isolated_workspace` audit section; `workspace_mode` defaults `"isolated"` and the three `orphan_*_count` default `0` (all always emitted).

**Fields**

| name | type | vis |
|------|------|-----|
| `operation_id` | `Option<String>` | `pub` |
| `workspace_mode` | `String` | `pub` |
| `workspace_handle_id` | `Option<String>` | `pub` |
| `agent_id` | `Option<String>` | `pub` |
| `holder_pid` | `Option<i64>` | `pub` |
| `holder_pid_alive` | `Option<bool>` | `pub` |
| `cgroup_id` | `Option<String>` | `pub` |
| `cgroup_removed` | `Option<bool>` | `pub` |
| `scratch_removed` | `Option<bool>` | `pub` |
| `upperdir_bytes` | `Option<i64>` | `pub` |
| `upperdir_cap_bytes` | `Option<i64>` | `pub` |
| `memory_current_bytes` | `Option<i64>` | `pub` |
| `memory_peak_bytes` | `Option<i64>` | `pub` |
| `cpu_usage_usec_delta` | `Option<i64>` | `pub` |
| `orphan_holder_count` | `i64` | `pub` |
| `orphan_cgroup_count` | `i64` | `pub` |
| `orphan_scratch_count` | `i64` | `pub` |
| `sampled_at_monotonic_s` | `Option<f64>` | `pub` |

<details><summary>Methods (1)</summary>

`default`

</details>

#### `OccSection`  ·  _struct_  ·  derives: `Debug, Clone, Default, PartialEq, Serialize, Deserialize`  ·  [L215]

`occ` audit section (PORT audit_schema.py:142-164).

**Fields**

| name | type | vis |
|------|------|-----|
| `operation_id` | `Option<String>` | `pub` |
| `operation_step` | `Option<i64>` | `pub` |
| `changeset_id` | `Option<String>` | `pub` |
| `changed_path_count` | `Option<i64>` | `pub` |
| `transaction_lock_wait_ms` | `Option<f64>` | `pub` |
| `prepare_ms` | `Option<f64>` | `pub` |
| `apply_ms` | `Option<f64>` | `pub` |
| `commit_ms` | `Option<f64>` | `pub` |
| `committed_layer_id` | `Option<String>` | `pub` |
| `publish_layer_ms` | `Option<f64>` | `pub` |
| `committed_layer_bytes` | `Option<i64>` | `pub` |
| `conflict_kind` | `Option<String>` | `pub` |
| `conflict_path` | `Option<String>` | `pub` |
| `conflict_reason` | `Option<String>` | `pub` |
| `base_manifest_version` | `Option<i64>` | `pub` |
| `current_manifest_version` | `Option<i64>` | `pub` |

#### `PluginSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  [L253]

`plugin` audit section; `plugin_id`/`plugin_kind` are required (always present).

**Fields**

| name | type | vis |
|------|------|-----|
| `plugin_id` | `String` | `pub` |
| `plugin_kind` | `String` | `pub` |
| `plugin_version` | `Option<String>` | `pub` |
| `plugin_tool_name` | `Option<String>` | `pub` |
| `request_bytes` | `Option<i64>` | `pub` |
| `response_bytes` | `Option<i64>` | `pub` |
| `duration_ms` | `Option<f64>` | `pub` |
| `status` | `Option<String>` | `pub` |
| `error_kind` | `Option<String>` | `pub` |
| `message_hash` | `Option<String>` | `pub` |
| `workspace_handle_id` | `Option<String>` | `pub` |
| `agent_id` | `Option<String>` | `pub` |
| `peak_resident_bytes` | `Option<i64>` | `pub` |

#### `BackgroundToolSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  [L283]

`background_tool` audit section; `background_task_id` required.

**Fields**

| name | type | vis |
|------|------|-----|
| `background_task_id` | `String` | `pub` |
| `task_kind` | `Option<String>` | `pub` |
| `tool_name` | `Option<String>` | `pub` |
| `agent_id` | `Option<String>` | `pub` |
| `uptime_ms` | `Option<f64>` | `pub` |
| `status` | `Option<String>` | `pub` |
| `exit_code` | `Option<i64>` | `pub` |
| `duration_ms` | `Option<f64>` | `pub` |
| `error_kind` | `Option<String>` | `pub` |
| `cancel_reason` | `Option<String>` | `pub` |
| `delivery_latency_ms` | `Option<f64>` | `pub` |

#### `ToolCallSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  [L310]

`tool_call` audit section; `tool_use_id`/`tool_name` required.

**Fields**

| name | type | vis |
|------|------|-----|
| `tool_use_id` | `String` | `pub` |
| `tool_name` | `String` | `pub` |
| `agent_id` | `Option<String>` | `pub` |
| `workspace_mode` | `Option<String>` | `pub` |
| `workspace_handle_id` | `Option<String>` | `pub` |
| `phase` | `Option<String>` | `pub` |
| `duration_ms` | `Option<f64>` | `pub` |
| `total_ms` | `Option<f64>` | `pub` |
| `exit_status` | `Option<String>` | `pub` |
| `bytes_in` | `Option<i64>` | `pub` |
| `bytes_out` | `Option<i64>` | `pub` |
| `phase_totals_rollup` | `Option<std::collections::BTreeMap<String, f64>>` | `pub` |

#### `OsResourceSection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  [L338]

`os_resource` audit section; `sampled_at_monotonic_s` required.

**Fields**

| name | type | vis |
|------|------|-----|
| `sampled_at_monotonic_s` | `f64` | `pub` |
| `rss_bytes` | `Option<i64>` | `pub` |
| `cpu_user_s` | `Option<f64>` | `pub` |
| `cpu_system_s` | `Option<f64>` | `pub` |
| `cpu_throttled_us` | `Option<i64>` | `pub` |
| `io_read_bytes` | `Option<i64>` | `pub` |
| `io_write_bytes` | `Option<i64>` | `pub` |
| `io_read_ops` | `Option<i64>` | `pub` |
| `io_write_ops` | `Option<i64>` | `pub` |

---

## `eos-protocol/src/cas.rs`

#### `CasError`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Error`  ·  `#[non_exhaustive]`  ·  [L32]

Errors raised while parsing CAS path / manifest values.

**Variants**: `InvalidPath(String)`, `UnsupportedSchemaVersion(i64)`

#### `LayerPath`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash`  ·  [L46]

A normalized, relative, NUL-free layer path (`api-parse-dont-validate`); construct via `LayerPath::parse`, an invalid path is unrepresentable.

**Fields**

| name | type | vis |
|------|------|-----|
| `0` | `String` |  |

<details><summary>Methods (3)</summary>

`parse`, `as_str`, `fmt`

</details>

#### `LayerRef`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L99]

One layer reference in a manifest: `{layer_id, path}` (both strings).

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_id` | `String` | `pub` |
| `path` | `String` | `pub` |

#### `Manifest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L108]

The persisted manifest; `version`/`schema_version` are NOT hashed by `manifest_root_hash`, only `layers` (in given order) is.

**Fields**

| name | type | vis |
|------|------|-----|
| `version` | `i64` | `pub` |
| `layers` | `Vec<LayerRef>` | `pub` |
| `schema_version` | `i64` | `pub` |

<details><summary>Methods (2)</summary>

`new`, `depth`

</details>

#### `LayerChange`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L224]

A storage-level layer change; tagged union by kind, `path` is the post-normalization form and `Write` carries raw bytes hashed verbatim.

**Variants**: `Write { path: LayerPath, content: Vec<u8> }`, `Delete { path: LayerPath }`, `Symlink { path: LayerPath, source_path: String }`, `OpaqueDir { path: LayerPath }`

<details><summary>Methods (2)</summary>

`kind`, `path`

</details>

---

## `eos-protocol/src/envelope.rs`

#### `ProtocolError`  ·  _enum_  ·  derives: `Debug, Error`  ·  `#[non_exhaustive]`  ·  [L22]

Encode/decode failures for the framed wire protocol; distinct from the wire `ErrorKind` (daemon policy, not a transport parse failure).

**Variants**: `BadJson(serde_json::Error)`, `NotAnObject`

#### `Request`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L37]

Request envelope (host -> daemon): `{op, invocation_id, args}`; field order on the wire is exactly this.

**Fields**

| name | type | vis |
|------|------|-----|
| `op` | `String` | `pub` |
| `invocation_id` | `String` | `pub` |
| `args` | `Value` | `pub` |

#### `ErrorEnvelope`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L47]

Daemon error envelope (`success:false`); `warnings`/`timings` are always `[]`/`{}` at the builder.

**Fields**

| name | type | vis |
|------|------|-----|
| `success` | `bool` | `pub` |
| `warnings` | `Vec<Value>` | `pub` |
| `timings` | `serde_json::Map<String, Value>` | `pub` |
| `error` | `ErrorBody` | `pub` |

#### `ErrorBody`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L58]

The `error` body of an `ErrorEnvelope`.

**Fields**

| name | type | vis |
|------|------|-----|
| `kind` | `ErrorKind` | `pub` |
| `message` | `String` | `pub` |
| `details` | `Value` | `pub` |

#### `ErrorKind`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  `#[serde(rename_all = "snake_case")]`, `#[non_exhaustive]`  ·  [L71]

Verified daemon error `kind` values; serialized `snake_case` on the wire.

**Variants**: `InvalidEnvelope`, `BadJson`, `RequestTooLarge`, `Unauthorized`, `UnknownOp`, `InternalError`, `Forbidden`, `ForbiddenInIsolatedWorkspace`, `LifecycleInProgress`

#### `Envelope`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  `#[serde(untagged)]`  ·  [L97]

A framed wire message: a request, an error envelope, or any response `Value`; untagged disambiguation by shape.

**Variants**: `Request(Request)`, `Error(ErrorEnvelope)`, `Response(Value)`

---

## `eos-protocol/src/models.rs`

#### `Intent`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  `#[serde(rename_all = "snake_case")]`  ·  [L28]

The single enum in the verb model; serialized as its `.value` string.

**Variants**: `ReadOnly`, `WriteAllowed`, `Lifecycle`

#### `ConflictInfo`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L40]

`{reason, conflict_file, message}` — serialized verbatim into guarded results.

**Fields**

| name | type | vis |
|------|------|-----|
| `reason` | `String` | `pub` |
| `conflict_file` | `Option<String>` | `pub` |
| `message` | `String` | `pub` |

<details><summary>Methods (2)</summary>

`rejected`, `overlap`

</details>

#### `ReadFileArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L79]

`read_file` request args.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `String` | `pub` |

#### `WriteFileArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L86]

`write_file` request args; `overwrite` defaults `true` at the primitive.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `String` | `pub` |
| `content` | `String` | `pub` |
| `overwrite` | `bool` | `pub` |

#### `EditFileArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L94]

`edit_file` request args.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `String` | `pub` |
| `edits` | `Vec<SearchReplaceEdit>` | `pub` |

#### `CommandOutput`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L101]

Public command output payload.

**Fields**

| name | type | vis |
|------|------|-----|
| `stdout` | `String` | `pub` |
| `stderr` | `String` | `pub` |

#### `ExecCommandArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L108]

`exec_command` request args.

**Fields**

| name | type | vis |
|------|------|-----|
| `cmd` | `String` | `pub` |
| `yield_time_ms` | `Option<u64>` | `pub` |
| `timeout_seconds` | `Option<u64>` | `pub` |
| `max_output_tokens` | `Option<u64>` | `pub` |

#### `ExecCommandResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L120]

Public `exec_command` / command-session result payload.

**Fields**

| name | type | vis |
|------|------|-----|
| `status` | `String` | `pub` |
| `exit_code` | `Option<i64>` | `pub` |
| `output` | `CommandOutput` | `pub` |
| `command_session_id` | `Option<String>` | `pub` |

#### `CommandSessionWriteArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L130]

`write_stdin` request args.

**Fields**

| name | type | vis |
|------|------|-----|
| `command_session_id` | `String` | `pub` |
| `chars` | `String` | `pub` |
| `yield_time_ms` | `Option<u64>` | `pub` |
| `max_output_tokens` | `Option<u64>` | `pub` |

#### `CommandSessionCancelArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L141]

Internal `api.v1.command.cancel` request args.

**Fields**

| name | type | vis |
|------|------|-----|
| `command_session_id` | `String` | `pub` |

#### `GlobArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L148]

`glob` request args; `path` sent only when non-None.

**Fields**

| name | type | vis |
|------|------|-----|
| `pattern` | `String` | `pub` |
| `path` | `Option<String>` | `pub` |

#### `GrepArgs`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L158]

`grep` request args; `path`/`glob_filter`/`head_limit` sent only when non-None, `head_limit`/`offset` are wire-present but primitive-inert.

**Fields**

| name | type | vis |
|------|------|-----|
| `pattern` | `String` | `pub` |
| `output_mode` | `String` | `pub` |
| `offset` | `i64` | `pub` |
| `case_insensitive` | `bool` | `pub` |
| `line_numbers` | `bool` | `pub` |
| `multiline` | `bool` | `pub` |
| `path` | `Option<String>` | `pub` |
| `glob_filter` | `Option<String>` | `pub` |
| `head_limit` | `Option<i64>` | `pub` |

#### `ReadFileResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L176]

`read_file` response (`SandboxResultBase` + content/exists/encoding).

**Fields**

| name | type | vis |
|------|------|-----|
| `success` | `bool` | `pub` |
| `workspace` | `String` | `pub` |
| `timings` | `Map<String, Value>` | `pub` |
| `conflict` | `Option<ConflictInfo>` | `pub` |
| `conflict_reason` | `Option<String>` | `pub` |
| `changed_paths` | `Vec<String>` | `pub` |
| `error` | `Option<Value>` | `pub` |
| `content` | `String` | `pub` |
| `exists` | `bool` | `pub` |
| `encoding` | `String` | `pub` |

#### `WriteFileResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L192]

`write_file` response (`GuardedResultBase`, no added fields).

**Fields**

| name | type | vis |
|------|------|-----|
| `success` | `bool` | `pub` |
| `workspace` | `String` | `pub` |
| `timings` | `Map<String, Value>` | `pub` |
| `conflict` | `Option<ConflictInfo>` | `pub` |
| `conflict_reason` | `Option<String>` | `pub` |
| `changed_paths` | `Vec<String>` | `pub` |
| `error` | `Option<Value>` | `pub` |
| `changed_path_kinds` | `Map<String, Value>` | `pub` |
| `mutation_source` | `String` | `pub` |
| `status` | `String` | `pub` |

#### `EditFileResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L208]

`edit_file` response (`GuardedResultBase` + `applied_edits`).

**Fields**

| name | type | vis |
|------|------|-----|
| `success` | `bool` | `pub` |
| `workspace` | `String` | `pub` |
| `timings` | `Map<String, Value>` | `pub` |
| `conflict` | `Option<ConflictInfo>` | `pub` |
| `conflict_reason` | `Option<String>` | `pub` |
| `changed_paths` | `Vec<String>` | `pub` |
| `error` | `Option<Value>` | `pub` |
| `changed_path_kinds` | `Map<String, Value>` | `pub` |
| `mutation_source` | `String` | `pub` |
| `status` | `String` | `pub` |
| `applied_edits` | `i64` | `pub` |

#### `GlobResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L225]

`glob` response (`SandboxResultBase` + `filenames/num_files/truncated`).

**Fields**

| name | type | vis |
|------|------|-----|
| `success` | `bool` | `pub` |
| `workspace` | `String` | `pub` |
| `timings` | `Map<String, Value>` | `pub` |
| `conflict` | `Option<ConflictInfo>` | `pub` |
| `conflict_reason` | `Option<String>` | `pub` |
| `changed_paths` | `Vec<String>` | `pub` |
| `error` | `Option<Value>` | `pub` |
| `filenames` | `Vec<String>` | `pub` |
| `num_files` | `i64` | `pub` |
| `truncated` | `bool` | `pub` |

#### `GrepResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L241]

`grep` response (`SandboxResultBase` + grep counters/content).

**Fields**

| name | type | vis |
|------|------|-----|
| `success` | `bool` | `pub` |
| `workspace` | `String` | `pub` |
| `timings` | `Map<String, Value>` | `pub` |
| `conflict` | `Option<ConflictInfo>` | `pub` |
| `conflict_reason` | `Option<String>` | `pub` |
| `changed_paths` | `Vec<String>` | `pub` |
| `error` | `Option<Value>` | `pub` |
| `output_mode` | `String` | `pub` |
| `filenames` | `Vec<String>` | `pub` |
| `content` | `String` | `pub` |
| `num_files` | `i64` | `pub` |
| `num_lines` | `i64` | `pub` |
| `num_matches` | `i64` | `pub` |
| `applied_limit` | `Option<i64>` | `pub` |
| `applied_offset` | `i64` | `pub` |
| `truncated` | `bool` | `pub` |

#### `SearchReplaceEdit`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L263]

A single search/replace edit on the wire: `{old_text, new_text, replace_all}`.

**Fields**

| name | type | vis |
|------|------|-----|
| `old_text` | `String` | `pub` |
| `new_text` | `String` | `pub` |
| `replace_all` | `bool` | `pub` |

#### `SearchReplaceError`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Error`  ·  `#[non_exhaustive]`  ·  [L274]

Failure of `apply_search_replace`; the message strings are part of the contract.

**Variants**: `EmptyAnchor`, `NotFound`, `CountMismatch`
