# Crate `eos-sandbox-host` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-sandbox-host/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**23 types across 7 files.**

The `eos-sandbox-host` crate owns the host side of the sandbox: it uses Docker as the only Rust production provider, holds the per-process [`ProviderRegistry`] as explicit application state, runs container lifecycle with post-lifecycle bootstrap, transports JSON envelopes to the resident in-sandbox `eosd` daemon with spawn/connect/empty-response recovery and typed error decoding, and uploads + verifies the pinned `eosd` runtime artifact. The provider seam is the sealed [`ProviderAdapter`] trait (its [`Sealed`](#sealed--trait--pubcrate--l294) supertrait lives in a `pub(crate)` module so no downstream crate can implement it), with the concrete [`DockerProviderAdapter`] over `bollard` and a `#[cfg(test)]` mock substitutable behind `Arc<dyn ProviderAdapter>`. Central types are [`DaemonClient`] (the daemon-backed `SandboxTransport` implementor), [`SandboxLifecycle`] (container CRUD + `setup_post_lifecycle`), [`RequestSandboxProvisioner`] (request-scoped `prepare_for_run`), the [`ProviderRegistry`], the provider value types (`CreateSandboxSpec`, `SandboxInfo`, `DaemonTcpEndpoint`, `RawExecResult`, `ProviderHealth`, `PreviewUrl`, `SnapshotInfo`, the `ContextPreparer` fixed point), the `ProviderKind` selector, and the single `SandboxHostError` enum. It implements `eos-sandbox-api`'s `SandboxTransport` (over `DaemonOp`/`SandboxApiError`) and depends on `eos-types`, `eos-config`, `eos-protocol` (the sibling `sandbox/` workspace's wire-protocol crate — the single source for the daemon protocol version, field names, exit codes, and reconnect schedule, consumed via a unilateral path edge so a daemon-side bump cannot silently drift the host), `bollard`, `async-trait`, `parking_lot`, `tokio`, `serde`/`serde_json`, `schemars`, `sha2`, and `tar`; `eos-runtime` is its consumer — it injects an `Arc<dyn SandboxTransport>` (a `DaemonClient`) and wraps `RequestSandboxProvisioner` at the composition root.

## Contents

- **`eos-sandbox-host/src/daemon_client.rs`** — `DaemonClient`, `TcpError`
- **`eos-sandbox-host/src/docker.rs`** — `DockerProviderAdapter`
- **`eos-sandbox-host/src/error.rs`** — `SandboxHostError`
- **`eos-sandbox-host/src/lifecycle.rs`** — `LifecyclePhase`, `SandboxLifecycle`
- **`eos-sandbox-host/src/provider.rs`** — `Labels`, `ProviderKind`, `CreateSandboxSpec`, `SandboxInfo`, `DaemonTcpEndpoint`, `RawExecResult`, `ExecOpts`, `ProviderHealth`, `PreviewUrl`, `SnapshotInfo`, `ContextPreparer`, `DockerContextPreparer`, `Sealed`, `ProviderAdapter`
- **`eos-sandbox-host/src/provisioning.rs`** — `RequestSandboxBinding`, `RequestSandboxProvisioner`
- **`eos-sandbox-host/src/registry.rs`** — `ProviderRegistry`

---

## `eos-sandbox-host/src/daemon_client.rs`

#### `DaemonClient`  ·  _struct_  ·  derives: `Debug`  ·  [L79]

The daemon-backed `SandboxTransport` implementor: resolves the provider adapter, runs the recovery state machine, decodes the typed response, and owns the per-sandbox TCP-endpoint cache + single-flight locks.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `registry` | `Arc<ProviderRegistry>` |  |
| `tcp_cache` | `RwLock<HashMap<SandboxId, Option<DaemonTcpEndpoint>>>` |  |
| `tcp_locks` | `RwLock<HashMap<SandboxId, Arc<tokio::sync::Mutex<()>>>>` |  |

**Trait impls**: `SandboxTransport`

<details><summary>Methods (10)</summary>

`new`, `registry`, `invalidate_daemon_tcp_endpoint`, `call_daemon_api`, `ensure_daemon_current`, `call_daemon`, `dispatch_with_daemon_spawn_recovery`, `call_daemon_envelope_with_connect_retry`, `send_daemon_envelope`, `resolve_daemon_tcp_endpoint`

</details>

#### `TcpError`  ·  _enum_  ·  private  ·  [L725]

Internal TCP send-path failure category, mapped to the synthetic thin-client exit codes 97 (connect) / 98 (I/O).

**Variants**: `Connect(String)`, `Io(String)`

---

## `eos-sandbox-host/src/docker.rs`

#### `DockerProviderAdapter`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L50]

The Docker-backed provider adapter; holds a cheap-to-clone, pooled `bollard::Docker` handle.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `docker` | `Docker` |  |

**Trait impls**: `Sealed, ProviderAdapter`

<details><summary>Methods (5)</summary>

`connect`, `from_client`, `inspect`, `pull_image`, `exec_inner`

</details>

---

## `eos-sandbox-host/src/error.rs`

#### `SandboxHostError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L12]

Every fallible operation in `eos-sandbox-host` returns this one error enum.

**Variants**:
- `NoDefaultProvider` — `default()` was called before `set_default` seeded the registry.
- `UnknownSandbox(SandboxId)` — a typed lookup wanted an adapter for a sandbox with no binding and no default fallback.
- `UnknownProviderKind(String)` — provider selection resolved a non-Docker (or unknown) kind.
- `ExecFailed { exit_code: i32, message: String }` — a provider `exec` returned a non-zero exit the caller treats as fatal.
- `DaemonDispatch { kind: String, message: String, details: JsonObject }` — the daemon returned a non-policy `error` envelope for a dispatched op.
- `DaemonNotReady { details: JsonObject }` — a readiness probe reported the resident daemon is not ready.
- `BadResponse { stdout: String }` — the daemon thin-client produced output that did not decode to a JSON envelope.
- `ArtifactHashMismatch { arch: String, got: String, expected: String }` — the pinned `eosd` artifact failed its sha256 check before upload.
- `ArtifactMissing { arch: String }` — no pinned `eosd` artifact exists for the resolved architecture.
- `UnsupportedArchitecture { machine: String }` — `uname -m` reported a machine the host cannot map to an `eosd` arch.
- `InvalidRequest(String)` — a request carried contradictory or missing required arguments (the `#[non_exhaustive]` growth slot for argument-validation failures).
- `Docker(#[source] bollard::errors::Error)` — a `bollard` Docker Engine API call failed.
- `Io(#[from] std::io::Error)` — a transport / filesystem I/O error.
- `Json(#[from] serde_json::Error)` — a JSON (de)serialization error.

---

## `eos-sandbox-host/src/lifecycle.rs`

#### `LifecyclePhase`  ·  _enum_  ·  derives: `Debug, Clone, Copy`  ·  private  ·  [L34]

Which lifecycle entry point drove the post-lifecycle bootstrap (the setup sequence is identical for both).

**Variants**: `Create`, `Start`

#### `SandboxLifecycle`  ·  _struct_  ·  derives: `Debug`  ·  [L43]

Per-process container lifecycle orchestration over the provider registry + daemon client; holds no lock itself.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `daemon` | `Arc<DaemonClient>` |  |
| `artifact_dir` | `PathBuf` |  |

<details><summary>Methods (12)</summary>

`new`, `create`, `start`, `stop`, `delete`, `set_labels`, `ensure_running`, `setup_post_lifecycle`, `start_runtime_bundle_upload`, `run_runtime_bootstrap`, `ensure_workspace_base`, `ensure_git`

</details>

---

## `eos-sandbox-host/src/provider.rs`

#### `Labels`  ·  _type alias_  ·  = `BTreeMap<String, String>`  ·  [L20]

Container/sandbox label map (`BTreeMap` for deterministic order).

#### `ProviderKind`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L28]

The sandbox backend selector; the Rust migration ships only `Docker`.

**Variants**: `Docker` (`#[default]`)

<details><summary>Methods (1)</summary>

`as_str`

</details>

#### `CreateSandboxSpec`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L47]

Arguments to `ProviderAdapter::create` (mirrors the Python `create(...)` kwargs; `language` defaults to `"python"`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `String` | `pub` |
| `snapshot` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `image` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `language` | `String` | `pub` · `#[serde(default = "default_language")]` |
| `env_vars` | `BTreeMap<String, String>` | `pub` · `#[serde(default)]` |
| `labels` | `Labels` | `pub` · `#[serde(default)]` |
| `platform` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |

**Trait impls**: `Default`

#### `SandboxInfo`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L93]

Canonical serialized sandbox/container shape returned by the provider.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `SandboxId` | `pub` |
| `name` | `String` | `pub` |
| `image` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `snapshot` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `state` | `String` | `pub` |
| `labels` | `Labels` | `pub` · `#[serde(default)]` |
| `project_dir` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `managed_by_app` | `bool` | `pub` |

#### `DaemonTcpEndpoint`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L119]

Docker host-side TCP path to the resident daemon (from `get_daemon_tcp_endpoint`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `host` | `String` | `pub` |
| `port` | `u16` | `pub` |
| `internal_port` | `Option<u16>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `auth_token` | `String` | `pub` |

#### `RawExecResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L135]

The `ProviderAdapter::exec` return — owned here (sandbox-api drops it as "a host concern").

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `exit_code` | `i32` | `pub` |
| `stdout` | `String` | `pub` |
| `stderr` | `String` | `pub` · `#[serde(default)]` |
| `success` | `bool` | `pub` · `#[serde(default = "default_true")]` |

**Trait impls**: `Default`

#### `ExecOpts`  ·  _struct_  ·  derives: `Debug, Clone, Default, PartialEq, Eq`  ·  [L165]

Options for a provider `exec`; not a wire DTO (carries a `Duration`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `cwd` | `Option<String>` | `pub` |
| `timeout` | `Option<Duration>` | `pub` |

#### `ProviderHealth`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L174]

Provider health snapshot (mirrors the Docker `get_health` dict).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `provider` | `String` | `pub` |
| `healthy` | `bool` | `pub` |
| `server_version` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `containers_running` | `Option<u64>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `kernel_version` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `operating_system` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `error` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |

#### `PreviewUrl`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L199]

A signed-preview-URL result; Docker returns `{ url: None, reason }`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `url` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `reason` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |

#### `SnapshotInfo`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema`  ·  [L211]

A provider snapshot/image listing entry (mirrors Docker `_serialize_image`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `image` | `Option<String>` | `pub` · `#[serde(skip_serializing_if = "Option::is_none", default)]` |
| `id` | `String` | `pub` |
| `tags` | `Vec<String>` | `pub` · `#[serde(default)]` |

#### `ContextPreparer`  ·  _enum_  ·  derives: `Debug, Clone`  ·  #[non_exhaustive]  ·  [L229]

The typed context-preparer fixed point (GC-07): a closed enum, not a new trait seam.

**Variants**: `Docker(DockerContextPreparer)`

<details><summary>Methods (2)</summary>

`prepare_context`, `prepare_context_async`

</details>

#### `DockerContextPreparer`  ·  _struct_  ·  derives: `Debug, Clone`  ·  #[non_exhaustive]  ·  [L269]

Docker context-preparer payload (GC-07 typed fixed point); carries the sandbox id and injects provider-neutral metadata into a tool context map.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `sandbox_id` | `SandboxId` |  |

<details><summary>Methods (2)</summary>

`new`, `inject`

</details>

#### `Sealed`  ·  _trait_  ·  pub(crate)  ·  [L294]

Seals `ProviderAdapter` (`api-sealed-trait`): declared `pub` inside the `pub(crate) mod sealed`, so only in-crate types can name and implement it.

#### `ProviderAdapter`  ·  _trait_  ·  bases: `sealed::Sealed + Send + Sync + std::fmt::Debug`  ·  async  ·  [L307]

Container CRUD + exec primitives implemented by each sandbox provider (the OCP/LSP seam); sealed and `#[async_trait]` because it is stored as `Arc<dyn ProviderAdapter>`.

**Trait items**:
- `fn kind(&self) -> ProviderKind;`
- `async fn health(&self) -> Result<ProviderHealth, SandboxHostError>;`
- `async fn list_snapshots(&self) -> Result<Vec<SnapshotInfo>, SandboxHostError>;`
- `async fn create(&self, spec: &CreateSandboxSpec) -> Result<SandboxInfo, SandboxHostError>;`
- `async fn get(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError>;`
- `async fn list(&self) -> Result<Vec<SandboxInfo>, SandboxHostError>;`
- `async fn start(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError>;`
- `async fn stop(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError>;`
- `async fn delete(&self, id: &SandboxId) -> Result<(), SandboxHostError>;`
- `async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<SandboxInfo, SandboxHostError>;`
- `async fn signed_preview_url(&self, id: &SandboxId, port: u16) -> Result<PreviewUrl, SandboxHostError>;`
- `async fn build_logs_url(&self, id: &SandboxId) -> Result<Option<String>, SandboxHostError>;`
- `async fn daemon_tcp_endpoint(&self, id: &SandboxId) -> Result<Option<DaemonTcpEndpoint>, SandboxHostError>;` (default)
- `async fn exec(&self, id: &SandboxId, command: &str, opts: &ExecOpts) -> Result<RawExecResult, SandboxHostError>;`
- `async fn put_archive(&self, id: &SandboxId, tar_stream: &[u8], dest_dir: &str) -> Result<(), SandboxHostError>;`
- `fn context_preparer(&self, id: &SandboxId) -> ContextPreparer;`

---

## `eos-sandbox-host/src/provisioning.rs`

#### `RequestSandboxBinding`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L22]

The resolved sandbox↔request binding produced by the provisioner.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `sandbox_id` | `SandboxId` | `pub` |
| `request_id` | `RequestId` | `pub` |

#### `RequestSandboxProvisioner`  ·  _struct_  ·  derives: `Debug`  ·  [L47]

Provisions the sandbox a request runs in, over the typed lifecycle seam.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `lifecycle` | `Arc<SandboxLifecycle>` |  |

<details><summary>Methods (2)</summary>

`new`, `prepare_for_run`

</details>

---

## `eos-sandbox-host/src/registry.rs`

#### `ProviderRegistry`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L54]

Process-local provider adapter registry, held as `Arc<ProviderRegistry>` and seeded once at the composition root.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `default` | `RwLock<Option<Arc<dyn ProviderAdapter>>>` |  |
| `bindings` | `RwLock<HashMap<SandboxId, Arc<dyn ProviderAdapter>>>` |  |

<details><summary>Methods (7)</summary>

`new`, `set_default`, `default`, `register`, `has`, `adapter`, `dispose`

</details>
