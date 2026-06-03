# Crate `eos-plugin` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-plugin/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**18 items (11 structs, 6 enums, 0 traits, 1 type alias) across 7 files.**

`eos-plugin` owns the pure plugin PPC contract layer of the eosd runtime: plugin/service manifests, validated service keys and status, daemon-to-harness refresh messages, public op-name registration, and bidirectional message-id'd PPC frames. It deliberately holds no process, overlay, OCC, or namespace state (those stay in `eos-daemon`), porting the Python `backend/src/sandbox/ephemeral_workspace/plugin` import-handler path into a typed daemon-owned service-process contract. Item groups: error surface (`error.rs`), manifests (`manifest.rs`), the PPC channel frame (`ppc.rs`), refresh protocol (`refresh.rs`), op registry (`registry.rs`), service identity (`service.rs`), and the logical service registry (`service_registry.rs`).

## Contents

- **`eos-plugin/src/error.rs`** — `PluginError`, `Result`
- **`eos-plugin/src/manifest.rs`** — `PluginManifest`, `PluginServiceManifest`, `PluginOperationManifest`
- **`eos-plugin/src/ppc.rs`** — `PpcDirection`, `PpcEnvelope`
- **`eos-plugin/src/refresh.rs`** — `RefreshRequest`, `RefreshAck`
- **`eos-plugin/src/registry.rs`** — `PluginOpRegistration`, `OpRegistry`
- **`eos-plugin/src/service.rs`** — `ServiceMode`, `RefreshStrategy`, `PluginServiceKey`, `PluginServiceKeyParts`
- **`eos-plugin/src/service_registry.rs`** — `PluginServiceState`, `PluginServiceStatus`, `PluginServiceRegistry`

---

## `eos-plugin/src/error.rs`

#### `PluginError`  ·  _enum_  ·  derives: `Debug, Error`  ·  `#[non_exhaustive]`  ·  [L13]

Failures surfaced by plugin contracts and the PPC channel; reproduces the Python `PluginOpRegistrationError`/`PluginOpConflictError`/`PluginEnsureError`/`RuntimeError` failure classes as a typed surface the daemon translates into the wire error envelope.

**Variants**: `Registration(String)`, `Conflict(String)`, `Ensure(String)`, `Manifest(String)`, `ProjectionStale(String)`, `Ppc(String)`, `ForbiddenInIsolatedWorkspace`

#### `Result`  ·  _type alias_  ·  `= core::result::Result<T, PluginError>`  ·  [L55]

Convenience alias for fallible plugin operations.

---

## `eos-plugin/src/manifest.rs`

#### `PluginManifest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L13]

Top-level plugin manifest consumed by `api.plugin.ensure`.

**Fields**

| name | type | vis |
|------|------|-----|
| `plugin_id` | `String` | `pub` |
| `plugin_version` | `String` | `pub` |
| `plugin_digest` | `String` | `pub` |
| `services` | `Vec<PluginServiceManifest>` | `pub` |
| `operations` | `Vec<PluginOperationManifest>` | `pub` |

<details><summary>Methods (1)</summary>

`validate`

</details>

#### `PluginServiceManifest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L63]

One service declared by a plugin payload.

**Fields**

| name | type | vis |
|------|------|-----|
| `service_id` | `String` | `pub` |
| `service_profile_digest` | `String` | `pub` |
| `service_mode` | `ServiceMode` | `pub` |
| `refresh_strategy` | `RefreshStrategy` | `pub` |
| `command` | `Vec<String>` | `pub` |
| `ppc_protocol_version` | `u32` | `pub` |

<details><summary>Methods (1)</summary>

`validate`

</details>

#### `PluginOperationManifest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L106]

One public `plugin.<plugin>.<op>` operation.

**Fields**

| name | type | vis |
|------|------|-----|
| `op_name` | `String` | `pub` |
| `intent` | `Intent` | `pub` |
| `auto_workspace_overlay` | `bool` | `pub` |
| `service_id` | `Option<String>` | `pub` |
| `timeout_ms` | `Option<u64>` | `pub` |

<details><summary>Methods (1)</summary>

`validate`

</details>

---

## `eos-plugin/src/ppc.rs`

#### `PpcDirection`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L29]

Direction of a PPC message on the bidirectional channel.

**Variants**: `Request`, `Reply`

#### `PpcEnvelope`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L44]

A message-id'd PPC frame; `op` carries the public op name (or a reply sentinel) and `body` is opaque JSON text so PPC does not parse operation-specific payload schemas.

**Fields**

| name | type | vis |
|------|------|-----|
| `message_id` | `String` | `pub` |
| `direction` | `PpcDirection` | `pub` |
| `op` | `String` | `pub` |
| `body` | `String` | `pub` |

<details><summary>Methods (4)</summary>

`encode`, `decode`, `to_envelope`, `from_envelope`

</details>

---

## `eos-plugin/src/refresh.rs`

#### `RefreshRequest`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  `#[serde(tag = "type", rename_all = "snake_case")]`  ·  `#[non_exhaustive]`  ·  [L15]

Request sent by the daemon to a plugin service harness during refresh.

**Variants**: `PrepareRefresh { target_manifest_key: String }`, `Quiesce { request_id: String }`, `SwapWorkspace { layer_paths: Vec<String>, workspace_root: String, manifest_key: String }`, `NotifyRefresh { changed_paths: Vec<String>, full_resync: bool }`, `Resume { request_id: String }`, `Restart { reason: String }`, `Health { manifest_key: String }`

<details><summary>Methods (1)</summary>

`manifest_key`

</details>

#### `RefreshAck`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L63]

Harness acknowledgement for a refresh request.

**Fields**

| name | type | vis |
|------|------|-----|
| `manifest_key` | `String` | `pub` |
| `accepted` | `bool` | `pub` |
| `reason` | `Option<String>` | `pub` |

<details><summary>Methods (1)</summary>

`require_manifest`

</details>

---

## `eos-plugin/src/registry.rs`

#### `PluginOpRegistration`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L38]

One pending plugin-op registration; the Rust daemon never holds a Python callable, so the importlib path is replaced by a PPC service process.

**Fields**

| name | type | vis |
|------|------|-----|
| `plugin_name` | `String` | `pub` |
| `op_name` | `String` | `pub` |
| `intent` | `Intent` | `pub` |
| `auto_workspace_overlay` | `bool` | `pub` |

<details><summary>Methods (2)</summary>

`new`, `public_op_name`

</details>

#### `OpRegistry`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L109]

The pending-registration table the decorator appends to and `flush` drains; keyed on `(plugin_name, op_name)` where identical re-registration is a no-op and a conflicting handler errors.

**Fields**

| name | type | vis |
|------|------|-----|
| `pending` | `Vec<PluginOpRegistration>` |  |

<details><summary>Methods (5)</summary>

`new`, `register`, `pending`, `clear`, `flush`

</details>

---

## `eos-plugin/src/service.rs`

#### `ServiceMode`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize`  ·  `#[serde(rename_all = "snake_case")]`  ·  `#[non_exhaustive]`  ·  [L15]

The daemon-managed service mode for long-lived read-only services.

**Variants**: `WorkspaceSnapshotRefresh`, `OneshotOverlay`

#### `RefreshStrategy`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize`  ·  `#[serde(rename_all = "snake_case")]`  ·  `#[non_exhaustive]`  ·  [L27]

Mechanism used when a `workspace_snapshot_refresh` service is stale.

**Variants**: `RemountWorkspaceAndNotify`, `RemountWorkspace`, `RestartService`

#### `PluginServiceKey`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize`  ·  [L45]

Stable key for sharing a daemon-managed plugin service; reuse is intentionally stricter than the old per-root cache so two payloads cannot accidentally share a process just because they share a `LayerStack` root.

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_stack_root` | `String` | `pub` |
| `workspace_root` | `String` | `pub` |
| `plugin_id` | `String` | `pub` |
| `plugin_digest` | `String` | `pub` |
| `service_id` | `String` | `pub` |
| `service_profile_digest` | `String` | `pub` |
| `service_mode` | `ServiceMode` | `pub` |
| `refresh_strategy` | `RefreshStrategy` | `pub` |

<details><summary>Methods (3)</summary>

`new`, `validate`, `service_instance_id`

</details>

#### `PluginServiceKeyParts`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L59]

Field bag used to construct `PluginServiceKey` without a long positional argument list.

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_stack_root` | `String` | `pub` |
| `workspace_root` | `String` | `pub` |
| `plugin_id` | `String` | `pub` |
| `plugin_digest` | `String` | `pub` |
| `service_id` | `String` | `pub` |
| `service_profile_digest` | `String` | `pub` |
| `service_mode` | `ServiceMode` | `pub` |
| `refresh_strategy` | `RefreshStrategy` | `pub` |

---

## `eos-plugin/src/service_registry.rs`

#### `PluginServiceState`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  `#[serde(rename_all = "snake_case")]`  ·  `#[non_exhaustive]`  ·  [L17]

Lifecycle state reported by a plugin service.

**Variants**: `Starting`, `Ready`, `Refreshing`, `Stale`, `Restarting`, `Stopped`, `Failed`

#### `PluginServiceStatus`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L29]

Serializable status for `api.plugin.status`.

**Fields**

| name | type | vis |
|------|------|-----|
| `key` | `PluginServiceKey` | `pub` |
| `state` | `PluginServiceState` | `pub` |
| `manifest_key` | `Option<String>` | `pub` |
| `registered_ops` | `Vec<String>` | `pub` |
| `refresh_count` | `u64` | `pub` |
| `restart_count` | `u64` | `pub` |
| `last_error` | `Option<String>` | `pub` |

<details><summary>Methods (2)</summary>

`new`, `require_ready_on_manifest`

</details>

#### `PluginServiceRegistry`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L83]

Pure registry keyed by `PluginServiceKey`; performs no process I/O while `eos-daemon` wraps it with live process, namespace, and PPC management.

**Fields**

| name | type | vis |
|------|------|-----|
| `services` | `BTreeMap<PluginServiceKey, PluginServiceStatus>` |  |

<details><summary>Methods (8)</summary>

`new`, `ensure`, `get`, `mark_ready`, `mark_stale`, `statuses`, `len`, `is_empty`

</details>
