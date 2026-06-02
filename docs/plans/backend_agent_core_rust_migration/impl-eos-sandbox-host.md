# impl-eos-sandbox-host — Docker sandbox adapter, lifecycle, and daemon transport host

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §12 (lines 826-882)
> and cross-cutting §Sandbox (lines 1134-1142).

## 1. Purpose & Responsibility (SRP)

`eos-sandbox-host` is the **host side** of the sandbox: it uses Docker as the
only Rust production sandbox provider, owns the per-process provider registry
**as explicit application state**, runs container lifecycle
(create/start/stop/delete/labels/ensure-running) with post-lifecycle setup,
transports JSON envelopes to the resident in-sandbox daemon with spawn/connect
recovery and typed error decoding, and uploads + verifies the pinned `eosd`
runtime artifact. It also exposes the request-scoped sandbox provisioner used by
the runtime entry path. A test mock may implement the sealed provider seam for
unit tests; any non-Docker production provider requires a separate plan.

This crate **must NOT**: reimplement LayerStack, OCC, overlay, or plugin
execution internals (those stay daemon-side — anchor §2 non-goal); define the
public sandbox tool request/result DTOs, `SandboxCaller`, daemon op-name
constants, or the `SandboxTransport` trait (owned by `eos-sandbox-api`, anchor
§5); hold a global agent orchestrator; or run a provider-level persistent shell
session (background execution is an engine dispatch mode, anchor §3). It builds
**on top of** the `SandboxTransport` abstraction and the typed `tool_api`; it
does not own them.

## 2. Dependencies

- **Upstream crates (depends on):**
  - `eos-types` — `SandboxId`, `RequestId`, `InvocationId`, `UtcDateTime`,
    `Clock`, `JsonObject`, `CoreError` (anchor §5).
  - `eos-config` — `CentralConfig.sandbox` section: Docker-only provider
    selection, Docker settings, and sandbox timeouts (anchor §5).
  - `eos-sandbox-api` — `SandboxCaller`, daemon op constants
    (`ops.rs`), the `SandboxTransport` trait, and the typed `tool_api` envelope
    parser. **This crate implements `SandboxTransport` via the registry+adapter
    pair**; it does not redefine those types (see impl-eos-sandbox-api.md).
    (`RawExecResult` is **not** from sandbox-api — that doc drops it as "a host
    concern"; it is owned here, see §5/§6.)
- **Downstream consumers (used by):**
  - `eos-runtime` — composition root constructs the `ProviderRegistry` app state,
    selects the provider from config, wires the `RequestSandboxProvisioner`, and
    drives lifecycle (anchor §5 row `eos-runtime`).
- **External crates** (pin via `[workspace.dependencies]`, inherit with
  `{ workspace = true }` — `proj-workspace-deps`):

| Crate | Justification | rust-skills |
|---|---|---|
| `tokio` (rt, net, io-util, time, process, sync) | async exec + AF_UNIX/TCP daemon transport, process spawn, bounded timeouts | `async-tokio-runtime` |
| `bollard` | typed async Docker Engine API client (container CRUD, exec, `put_archive`, port bindings) — avoids `Box<dyn>` over a hand-rolled HTTP client | `anti-type-erasure` |
| `async-trait` | `ProviderAdapter` is stored behind `Arc<dyn ProviderAdapter>` in the registry; native async-fn-in-trait is not yet `dyn`-safe (anchor §6) | `api-sealed-trait` |
| `parking_lot` | `RwLock` for the provider registry + TCP-endpoint cache map — synchronous read/insert, guard dropped before `.await`, `!Send` guard, no poison under `panic=unwind` (the across-await dedup guard stays `tokio::sync::Mutex`) | `own-mutex-interior`, anchor §7 |
| `serde` / `serde_json` | decode Docker JSON (container attrs, snapshots) and daemon envelope/response JSON | — |
| `thiserror` | the single `SandboxHostError` enum (`err-thiserror-lib`) | `err-thiserror-lib` |
| `sha2` | verify the pinned `eosd` artifact digest before upload | — |
| `tar` + `flate2` | build the eosd `put_archive` tar stream (Docker fast path) and the compat-bridge tarball | `mem-zero-copy` |
| `tracing` | structured spans for lifecycle/recovery (replaces `logging`) | — |
| `futures` | combinators for the connect-retry backoff stream | — |

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `sandbox/provider/protocol.py` | `provider.rs` | `ProviderAdapter` trait (owned here). Drop duck-typed `context_preparer(...) -> Any`: replace with the concrete `ContextPreparer` enum (GC-07) — a typed fixed point, not a new trait seam. |
| `sandbox/provider/bootstrap.py` | folded into `registry.rs` + `eos-runtime` wiring | First-call-wins process global + sentinel → **explicit `ProviderRegistry` app state** built once at the composition root (GC-02). Drop `_reset_for_tests`, the `threading.Lock` sentinel, env warning-on-mismatch. |
| `sandbox/provider/registry.py` | `registry.rs` | `set/get_default_provider`, `register/get/has/dispose_adapter`, **WR-01 no-cache-fallback** semantics preserved. `threading.Lock` → `parking_lot::RwLock` (synchronous read/insert, anchor §7). |
| `sandbox/provider/docker/adapter.py` | `docker.rs` | `DockerProviderAdapter` over `bollard`. Keep daemon-TCP endpoint derivation (`get_daemon_tcp_endpoint`), label conventions, `put_archive`. `asyncio.to_thread` Docker calls become native bollard async. |
| non-Docker provider modules | **not ported** | Rust agent-core is Docker-only for sandbox execution. Non-Docker Python provider paths remain outside this migration. |
| `sandbox/host/lifecycle.py` | `lifecycle.rs` | `create/start/stop/delete/set_labels/ensure_running` + post-lifecycle setup orchestration. The `delete` path's `forget_plugin_dispatch_state`/`forget_plugin_install_state` calls are host-local in-process cache cleanups (they `.pop()` from module-level dicts), **not** daemon ops — port as a local host-state cleanup, or drop if the Rust host holds no such cache (no plugin internals, no transport RPC). |
| `sandbox/host/isolated_workspace_lifecycle.py` | `isolated_workspace.rs` | Port host-side enter/exit lifecycle: reject enter with local/daemon in-flight ephemeral jobs or command sessions, invoke daemon enter/exit ops, cancel/drain per-agent background work before exit through the runtime adapter, release namespace/snapshot lease, and clear scratch state. Deep namespace/LayerStack implementation stays daemon-side. |
| `sandbox/host/bootstrap.py` | `lifecycle.rs` (setup) + `daemon_client.rs` (`ensure_workspace_base`) | Background tarball-upload thread-pool overlap → tokio `JoinSet` task (GC-05). `ensure_git`, readiness probes preserved. |
| `sandbox/host/daemon_client.py` | `daemon_client.rs` | Envelope build, spawn/connect/empty-response recovery state machine, TCP-endpoint cache, typed error decode. The Python/`eosd` spawn-command branching collapses: Rust default runtime is `eosd` (GC-04). |
| `sandbox/host/runtime_bundle.py` | `runtime_artifact.rs` (SHRUNK) | **Drop** the Python module-tarball builder (LayerStack/OCC/overlay/plugin/audit/pathspec vendoring). Keep only: pinned-`eosd` upload + sha verify + readiness, and a thin **compat-bridge** tarball upload retained behind a flag while Python sandboxes coexist (GC-01). |
| `sandbox/host/runtime_artifact/__init__.py` | `runtime_artifact.rs` (consts) | `EOSD_VERSION`, `EOSD_SHA256: {amd64,arm64}`, `MINISIGN_PUBLIC_KEY`, `PROTOCOL_VERSION` become `const`/`static`. |
| `sandbox/host/chunked_upload.py` | **not ported** | Docker `put_archive` is the only Rust upload path. |
| `runtime/sandbox_provisioning.py` | `provisioning.rs` | `RequestSandboxBinding`, `RequestSandboxProvisioner.prepare_for_run`. |

**In scope:** provider seam + Docker concrete + test mock, registry app state,
lifecycle, daemon transport + recovery, eosd artifact upload/verify/readiness,
request provisioning.
**Out of scope (daemon-side, do not port):** LayerStack, OCC, overlay pipeline,
plugin runtime, isolated-workspace `_control_plane` internals — the host only
issues daemon ops (`api.ensure_workspace_base`, `api.runtime.ready`, …).

## 4. File & Module Layout

```
src/
  lib.rs            // pub use of ProviderAdapter, ProviderKind, ProviderRegistry,
                    //   SandboxLifecycle, DaemonClient, RuntimeArtifact,
                    //   RequestSandboxProvisioner, SandboxHostError (proj-pub-use-reexport)
  error.rs          // SandboxHostError (the one thiserror enum) (pub(crate) ctors)
  provider.rs       // ProviderAdapter trait + ContextPreparer enum + ProviderKind + DaemonTcpEndpoint
  registry.rs       // ProviderRegistry app state (default + per-sandbox bindings)
  docker.rs         // DockerProviderAdapter (bollard)  (pub(crate) helpers)
  lifecycle.rs      // SandboxLifecycle: create/start/stop/delete/set_labels/ensure_running + setup
  daemon_client.rs  // DaemonClient: envelope dispatch, spawn/connect recovery, TCP cache, decode
  runtime_artifact.rs // eosd upload + sha verify + readiness; eosd consts; compat-bridge upload
  provisioning.rs   // RequestSandboxBinding, RequestSandboxProvisioner
```

`lib.rs` re-exports the public surface; adapter-internal serializers
(`serialize_container`) and the recovery helpers are
`pub(crate)` (`proj-pub-crate-internal`).

## 5. Contracts Owned Here

Owned by this crate (anchor §5 row `eos-sandbox-host`): the **`ProviderAdapter`
trait**, the **provider registry**, the Docker adapter, the daemon client,
lifecycle, runtime-artifact upload, and **`RawExecResult`** (the
`ProviderAdapter::exec` return — impl-eos-sandbox-api.md drops it as "a host
concern", and the host's `exec` is its sole producer; see §6). Fully specified
below.

### `ProviderAdapter` (the seam — OCP + LSP)

Sealed (`api-sealed-trait`) so only in-crate types implement it (Docker and a
test mock under `#[cfg(test)]`; future production providers require an explicit
plan);
`#[async_trait]` because it is stored as `Arc<dyn ProviderAdapter>` in the
registry (anchor §6 object-safety note). Provider lifecycle methods are async at
the seam; the Docker adapter awaits the native async `bollard` client directly.

```rust
mod sealed { pub trait Sealed {} }

#[async_trait::async_trait]
pub trait ProviderAdapter: sealed::Sealed + Send + Sync + std::fmt::Debug {
    fn kind(&self) -> ProviderKind;

    // health / discovery
    async fn health(&self) -> Result<ProviderHealth, SandboxHostError>;
    async fn list_snapshots(&self) -> Result<Vec<SnapshotInfo>, SandboxHostError>;

    // container CRUD
    async fn create(&self, spec: &CreateSandboxSpec) -> Result<SandboxInfo, SandboxHostError>;
    async fn get(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError>;
    async fn list(&self) -> Result<Vec<SandboxInfo>, SandboxHostError>;
    async fn start(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError>;
    async fn stop(&self, id: &SandboxId) -> Result<SandboxInfo, SandboxHostError>;
    async fn delete(&self, id: &SandboxId) -> Result<(), SandboxHostError>;
    async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<SandboxInfo, SandboxHostError>;

    // preview / observability
    async fn signed_preview_url(&self, id: &SandboxId, port: u16) -> Result<PreviewUrl, SandboxHostError>;
    async fn build_logs_url(&self, id: &SandboxId) -> Result<Option<String>, SandboxHostError>;
    /// Docker-only; default returns None for providers without a TCP daemon path.
    async fn daemon_tcp_endpoint(&self, id: &SandboxId) -> Result<Option<DaemonTcpEndpoint>, SandboxHostError> {
        let _ = id; Ok(None)
    }

    // exec + upload (the only async methods)
    async fn exec(&self, id: &SandboxId, command: &str, opts: &ExecOpts)
        -> Result<RawExecResult, SandboxHostError>; // RawExecResult owned here (§6)
    async fn put_archive(&self, id: &SandboxId, tar_stream: &[u8], dest_dir: &str)
        -> Result<(), SandboxHostError>;

    // context preparation (concrete enum; replaces duck-typed `context_preparer -> Any`, GC-07)
    fn context_preparer(&self, id: &SandboxId) -> ContextPreparer;
}
```

**Method-name mapping to the Python `ProviderAdapter` Protocol** (the `get_`
prefix is dropped per Rust API guidelines C-GETTER / `rust-skills`):
`health` ← `get_health`, `signed_preview_url` ← `get_signed_preview_url`,
`build_logs_url` ← `get_build_logs_url`, `daemon_tcp_endpoint` ←
`get_daemon_tcp_endpoint`, and `kind()` ← the `name: str` class attribute.

### `ContextPreparer` (concrete typed replacement for the `-> Any` hook, GC-07)

This is **not** a new trait seam (anchor §1/§6: only `ProviderAdapter + provider
registry` is on the map for this crate). GC-07 only requires a *typed fixed
point* per adapter for static analysis (the protocol.py rationale); a closed
enum gives that and tracks the Docker-only `ProviderKind` fixed point
(`type-enum-states`), so no `Box<dyn>` and no `#[async_trait]` are needed.

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ContextPreparer {
    Docker(DockerContextPreparer),
}

impl ContextPreparer {
    pub fn prepare_context(&self, ctx: &mut JsonObject) -> Result<(), SandboxHostError> { /* match self */ }
    pub async fn prepare_context_async(&self, ctx: &mut JsonObject) -> Result<(), SandboxHostError> { /* match self */ }
}
```
The Docker preparer payload (`DockerContextPreparer`) is a `pub(crate)` concrete
built by the Docker adapter's `context_preparer`.

### `ProviderRegistry` (explicit app state — GC-02)

```rust
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    default: parking_lot::RwLock<Option<Arc<dyn ProviderAdapter>>>,
    bindings: parking_lot::RwLock<HashMap<SandboxId, Arc<dyn ProviderAdapter>>>,
}
```
Constructed by `eos-runtime` and shared as `Arc<ProviderRegistry>`. Methods:
`set_default`, `default() -> Result<Arc<dyn ProviderAdapter>, SandboxHostError>`,
`register(id, adapter)`, `has(&id) -> bool`, `adapter(&id) -> Result<...>`,
`dispose(&id)`. **WR-01 preserved**: an unknown id falls back to the default
adapter **without caching** the association (so `has` keeps reporting `false`).

### `SandboxHostError` (the one error enum — §8 conventions)

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxHostError {
    #[error("no default sandbox provider registered")]
    NoDefaultProvider,
    #[error("no adapter for sandbox {0}")]
    UnknownSandbox(SandboxId),
    #[error("unknown sandbox provider {0:?}; expected docker")]
    UnknownProviderKind(String),
    #[error("provider exec failed (exit {exit_code}): {message}")]
    ExecFailed { exit_code: i32, message: String },
    #[error("daemon dispatch failed: {kind}: {message}")]
    DaemonDispatch { kind: String, message: String, details: JsonObject },
    #[error("daemon not ready")]
    DaemonNotReady { details: JsonObject },
    #[error("bad daemon response")]
    BadResponse { stdout: String },
    #[error("eosd artifact hash mismatch for {arch}: got {got}, expected {expected}")]
    ArtifactHashMismatch { arch: String, got: String, expected: String },
    #[error("eosd artifact missing for {arch}")]
    ArtifactMissing { arch: String },
    #[error("unsupported sandbox architecture for eosd artifact: {machine}")]
    UnsupportedArchitecture { machine: String },
    #[error("docker error")]
    Docker(#[source] bollard::errors::Error),
    #[error("transport io error")]
    Io(#[from] std::io::Error),
    #[error("json error")]
    Json(#[from] serde_json::Error),
}
```

**Used (not owned), referenced only:** `SandboxId`/`RequestId`/`InvocationId`/
`Clock`/`JsonObject`/`CoreError` (see impl-eos-types.md); `SandboxCaller`,
daemon op constants, `SandboxTransport`, typed `tool_api`
(see impl-eos-sandbox-api.md).

## 6. Types, Fields & Schemas

`ProviderKind` — the sandbox backend selector (anchor §4: `sandbox_provider`,
never bare `provider`). `#[non_exhaustive]` **is** applied so dispatch sites
outside this crate must already carry a catch-all arm, but the Rust migration
ships only the Docker variant.

| Variant | serde rename | Source of truth |
|---|---|---|
| `Docker` | `"docker"` | `DockerProviderAdapter.name`; Rust config accepts only Docker |

`CreateSandboxSpec` — args to `ProviderAdapter::create` (mirrors the Python
`create(...)` kwargs; `language` defaults to `"python"`).

| Field | Rust type | serde/schemars | Source |
|---|---|---|---|
| `name` | `String` | required | `create(name=)` |
| `snapshot` | `Option<String>` | skip_if none | `create(snapshot=)` |
| `image` | `Option<String>` | skip_if none | `create(image=)` |
| `language` | `String` (default `"python"`) | default | `create(language=)` |
| `env_vars` | `BTreeMap<String,String>` | default | `create(env_vars=)` |
| `labels` | `Labels` (= `BTreeMap<String,String>`) | default | `create(labels=)` |
| `platform` | `Option<String>` | skip_if none | `create(platform=)` (Docker only) |

`SandboxInfo` — canonical serialized Docker container shape.

| Field | Rust type | Notes / source |
|---|---|---|
| `id` | `SandboxId` | docker `id` |
| `name` | `String` | leading `/` stripped (docker) |
| `image` | `Option<String>` | docker `Config.Image` |
| `snapshot` | `Option<String>` | docker `labels["snapshot"]` |
| `state` | `String` | normalized lowercase (`status`/`state`) |
| `labels` | `Labels` | container/sandbox labels |
| `project_dir` | `Option<String>` | `labels["project_dir"]` or `WorkingDir` |
| `managed_by_app` | `bool` | canonical-normalized from `labels["managed_by"] == "eos"` |

Canonical-normalization drops Docker `_serialize_container.docker_init`
(`HostConfig.Init`) because no consumer in this crate needs it.

`DaemonTcpEndpoint` — Docker host-side TCP path to the resident daemon (from
`_DaemonTcpEndpoint`, docker `get_daemon_tcp_endpoint`).

| Field | Rust type | Source |
|---|---|---|
| `host` | `String` | `127.0.0.1` (mapped) |
| `port` | `u16` | host-mapped port (`HostPort`) |
| `internal_port` | `Option<u16>` | container port `37657` |
| `auth_token` | `String` | `EOS_DAEMON_AUTH_TOKEN` env |

`RawExecResult` — the `ProviderAdapter::exec` return, owned here (§5; sandbox-api
drops it). Mirrors `RawExecResult(SandboxResultBase)` in `sandbox/shared/models.py`,
keeping only the exec-relevant fields (the OCC/overlay base fields are daemon-side
and not produced by a raw provider exec).

| Field | Rust type | Notes / source |
|---|---|---|
| `exit_code` | `i32` | `RawExecResult.exit_code` |
| `stdout` | `String` | `RawExecResult.stdout` |
| `stderr` | `String` (default `""`) | `RawExecResult.stderr` |
| `success` | `bool` (default `true`) | `SandboxResultBase.success` |

`ExecOpts { cwd: Option<String>, timeout: Option<Duration> }`. `ProviderHealth`
mirrors Docker health fields: `{provider, healthy, server_version,
containers_running, kernel_version, operating_system}`. `PreviewUrl` keeps the
provider-neutral return shape but Docker returns `{url: None, reason: "docker
provider has no signed preview URL"}`. `SnapshotInfo` is a small serde struct
mirroring Docker image/snapshot listing. All
wire/DTO structs derive `Debug, Clone, PartialEq, Serialize, Deserialize,
JsonSchema` (`api-common-traits`); `CreateSandboxSpec`, `SandboxInfo`, and
`RawExecResult` derive `Default` where sensible (`api-default-impl`).

### Daemon transport constants (from `daemon_client.py` / `runtime_artifact`)

```rust
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;
const DAEMON_PROTOCOL_FIELD: &str = "_eos_daemon_protocol_version";
const DAEMON_AUTH_FIELD: &str = "_eos_daemon_auth_token";
const THIN_CLIENT_CONNECT_FAILED: i32 = 97;
const THIN_CLIENT_IO_FAILED: i32 = 98;
const EMPTY_RESPONSE_MESSAGE: &str = "EOS_DAEMON_IO_FAILED:empty_response";
const CONNECT_RETRY_DELAYS: [Duration; 4] =
    [ms(250), ms(500), ms(1000), ms(2000)];   // _CONNECT_RETRY_DELAYS_S
pub const EOSD_VERSION: &str = "0.1.0-local.20260602";
static EOSD_SHA256: &[(&str, &str)] =
    &[("amd64", "321efbd…fcfe"), ("arm64", "e07a595…417b")];
```

`MINISIGN_PUBLIC_KEY` (a `const` in the §3 source mapping) is intentionally
omitted from this block: the Python key is empty and minisign verify is deferred
(§8), so it carries no value to declare yet.

### Daemon envelope (request) / response decode

The host serializes one JSON envelope per call:
`{op, invocation_id, args}` with `args.layer_stack_root` injected
(`DEFAULT_LAYER_STACK_ROOT`) and, over TCP with a token, `_eos_daemon_auth_token`
added (`_authenticated_envelope_json`). `invocation_id` is generated fresh for
`api.v1.cancel`; otherwise taken from `args.invocation_id` or minted. The
response is JSON-decoded; a `response.error` that is **not** a handler-level
policy result (`success == false && non-empty status`) maps to
`SandboxHostError::DaemonDispatch`. `op` names come from `eos-sandbox-api::ops`
(referenced, not redefined).

## 7. Concurrency & State Ownership

Per anchor §7. Lower crate — **runtime-agnostic**: it never builds a Tokio
runtime; all methods are `&self`/`async fn` driven by `eos-runtime`'s single
multi-thread runtime.

- **`ProviderRegistry`** is the only shared mutable state: shared as
  `Arc<ProviderRegistry>` (`own-arc-shared`); `default` and `bindings` behind
  **`parking_lot::RwLock`** (reads dominate — every dispatch reads, registration
  is rare — `own-rwlock-readers`). The read path is synchronous: the
  `Arc<dyn ProviderAdapter>` is cloned out and the guard dropped **before** any
  `.await`, so the async `tokio::sync::RwLock` buys nothing (anchor §7); the
  `!Send` guard makes a hold-across-await a compile error
  (`async-no-lock-await`, `async-clone-before-await`).
- **Adapters** (`Arc<dyn ProviderAdapter>`) are immutable after construction;
  `bollard::Docker` is internally `Clone` + pooled.
- **Provider calls:** provider lifecycle methods are async at the seam, and the
  production Docker adapter awaits the native async `bollard` client directly.
- **TCP-endpoint cache** (`daemon_client.rs`): the
  `HashMap<SandboxId, Option<DaemonTcpEndpoint>>` lookup/insert is synchronous, so
  it sits behind a **`parking_lot::RwLock`**. The per-sandbox dedup guard is the
  **one `tokio::sync::Mutex`** in this crate, and deliberately so: it is **held
  across the async resolve round-trip** so concurrent callers single-flight rather
  than all hitting the daemon (the Python `_tcp_endpoint_cache_locks` pattern) —
  the legitimate must-span-`.await` case (anchor §7). Cache is invalidated on
  `CONNECT_FAILED` / empty TCP response.
- **TCP endpoint lock order:** registry locks are cloned-and-dropped before any
  adapter call; endpoint-cache read lock is dropped before acquiring the
  per-sandbox async mutex; after acquiring that mutex, recheck cache, await
  resolve, then take the write lock only to publish the result. No cache or
  registry guard is held across the resolve await.
- **Background bundle upload overlap** (post-create setup): the Python
  `ThreadPoolExecutor` + future-join becomes a single `tokio::task::JoinSet`
  task launched before `ensure_git`, drained (`join_next`) after, with errors
  swallowed-by-design so the sequential bootstrap retries (`async-joinset-structured`).
- **Timeouts:** every exec/transport call is wrapped in `tokio::time::timeout`
  (replaces `asyncio.wait_for`); connect-retry uses `tokio::time::sleep` over
  `CONNECT_RETRY_DELAYS`.
- No app-level mutex over the providers' own connection pools. The per-sandbox
  TCP single-flight mutex is the only lock deliberately held across `.await`;
  all other locks follow clone/extract/drop-before-await discipline.

## 8. Behavior & Invariants

- **Provider selection is app state, first-resolution-wins at the composition
  root** (plan GC: "model it as explicit app state in Rust", anchor §3). The
  Python `bootstrap_sandbox_provider` sentinel/global is replaced by building one
  `ProviderRegistry` and calling `set_default` once from `eos-runtime` after
  resolving `EOS_SANDBOX_PROVIDER` → `config.sandbox.default_provider`. An
  unknown or non-Docker kind fails fast (`SandboxHostError::UnknownProviderKind`
  — `api-parse-dont-validate`). `docker` is the only valid Rust provider kind.
- **Registry fallback no-cache (WR-01):** `adapter(unknown_id)` returns the
  default but does **not** insert it into `bindings`; `has(unknown_id)` stays
  `false`. This is a load-bearing invariant from `registry.py` (prevents
  unbounded cache growth and explicit-vs-fallback confusion).
- **Lifecycle order** (`lifecycle.py` / `bootstrap.py`): `create` →
  `register(id, default)` → post-create setup; `start` → post-start setup (same
  sequence). Setup sequence (`setup_post_lifecycle`): start bundle upload task →
  `ensure_git` (best-effort, install failures logged, adapter/config failures
  propagate) → drain upload → runtime bootstrap (eosd upload) →
  `ensure_workspace_base` (binding-mismatch → rebuild with `reset=true`, then
  `api.runtime.ready` must report `ready && control_plane ok && manifest_version >= 1`).
- **`delete`** runs host-local in-process plugin-cache cleanup (the Python
  `forget_plugin_dispatch_state`/`forget_plugin_install_state` `.pop()` from
  module-level dicts — a local registry/state cleanup, **not** a daemon RPC; drop
  it if the Rust host holds no such cache), then `dispose(id)` — without importing
  plugin internals.
- **Daemon recovery state machine** (`daemon_client.rs`, faithful port):
  1. Dispatch envelope via TCP if a cached endpoint exists, else AF_UNIX thin
     client through `adapter.exec`.
  2. On `CONNECT_FAILED`, or empty-response for a retry-eligible op, run the
     spawn command, then `api.runtime.ready` with connect-retry backoff, then
     replay the original envelope.
  3. **Empty-response retry is op-gated** (`_can_retry_empty_response`): mutating
     ops (`api.edit_file`, `api.v1.edit_file`, `api.write_file`,
     `api.v1.write_file`, `api.v1.exec_command`, `api.v1.exec_stdin`, any
     `plugin.*`) **fail closed** — never
     replayed (replay could convert an isolated in-flight call into a default-mode
     publish). Lifecycle/read/control ops retry.
  4. `api.ensure_workspace_base`/`api.build_workspace_base` may be declared ready
     despite a `control_plane WorkspaceBindingError` when every other probe is
     `ok` (`_is_bootstrap_ready_response`) — the original op then surfaces the
     binding failure on its own path.
- **Runtime default = Rust/`eosd` after migration** (plan §12 "Target should
  default to Rust daemon"). The Python-vs-`eosd` branching in
  `_daemon_spawn_command`/`_daemon_thin_client_command` collapses: the host emits
  the `eosd` spawn/thin-client commands by default; the Python launcher remains
  only behind the compat bridge (GC-04). The eosd spawn restarts the resident
  daemon when its env signature (`sandbox_runtime`, `runtime_bundle_sha`,
  `daemon_tcp_port`, `eosd_sha`) changes — preserved verbatim.
- **eosd artifact upload (`runtime_artifact.rs`):** probe `uname -m` → map to
  `amd64`/`arm64` (`x86_64`/`amd64` → `amd64`, `aarch64`/`arm64` → `arm64`;
  reject others with `SandboxHostError::UnsupportedArchitecture { machine }`,
  porting `_artifact_arch`'s `RuntimeError`); read the pinned `eosd-linux-{arch}`
  binary; verify sha256 against
  `EOSD_SHA256` (mismatch → `ArtifactHashMismatch`); skip if remote marker
  matches; upload via the Docker `put_archive` fast path; then `printf marker &&
  eosd --version` verification. Minisign verify
  is deferred (consts carry empty key — note, not implement).
- **`PROTOCOL_VERSION` lockstep:** `DAEMON_PROTOCOL_VERSION` must equal
  `runtime_artifact::PROTOCOL_VERSION` (compile-time `const _: () = assert!(...)`).

## 9. SOLID & Principles Applied

- **DIP:** the crate depends on the `SandboxTransport` abstraction (owned
  upstream) for tool dispatch and exposes the `ProviderAdapter` seam; `eos-runtime`
  injects the concrete registry/adapters (anchor §6).
- **OCP:** the provider seam is retained for the test mock and for a future
  explicit provider migration, but the production registry is seeded with Docker
  only. Non-Docker values fail at config/registry selection.
- **LSP:** the Docker adapter and a `#[cfg(test)]` mock are substitutable behind
  `Arc<dyn ProviderAdapter>` for lifecycle and daemon-client tests.
- **ISP:** the seam is the focused container+exec primitive set; orchestration
  (`SandboxLifecycle`, `DaemonClient`) sits on top in separate types, not on the
  adapter trait.
- **SRP:** provider primitives (`provider.rs`/`docker.rs`), app state
  (`registry.rs`), lifecycle policy (`lifecycle.rs`), transport+recovery
  (`daemon_client.rs`), artifact (`runtime_artifact.rs`), provisioning
  (`provisioning.rs`) are separate files.
- **KISS/YAGNI/DRY:** `ProviderKind` holds only Docker for Rust; the seam (trait
  + registry) is retained without inventing extra provider abstractions; the
  eosd-vs-Python branching is deleted in favor of the single Rust default plus
  one compat flag.
- **Non-goals respected (anchor §2):** no LayerStack/OCC/overlay/plugin
  reimplementation; no provider-level persistent shell session; no global
  orchestrator. `class_path` dynamic import never appears here.

## 10. Gap Closeouts (tracked requirements)

- **GC-eos-sandbox-host-01** — *runtime_bundle shrinks to artifact upload +
  compat bridge.* `runtime_artifact.rs` uploads only the pinned `eosd` binary
  (verify+readiness). The Python module-tarball builder
  (LayerStack/OCC/overlay/plugin/audit/pathspec vendoring) is dropped; a thin
  compat-bridge tarball upload is retained behind a `compat_python_bundle` flag,
  off by default, for the migration window only.
- **GC-eos-sandbox-host-02** — *provider registry is explicit app state.* Replace
  the `bootstrap.py` first-call-wins process global + sentinel with an
  `Arc<ProviderRegistry>` built and seeded once at the `eos-runtime` composition
  root; selection resolves `EOS_SANDBOX_PROVIDER` → `config.sandbox.default_provider`
  and accepts only Docker. No hidden process global.
- **GC-eos-sandbox-host-03** — *keep deep sandbox migration separate.* The host
  only issues daemon ops (`api.ensure_workspace_base`, `api.runtime.ready`,
  `api.build_workspace_base`) over the transport; the `delete`-path plugin-forget
  is host-local in-process cache cleanup, not a daemon op (see §8). No
  LayerStack/OCC/overlay/plugin code is ported (anchor §2 enforced by the file
  layout containing none of those modules).
- **GC-eos-sandbox-host-04** — *daemon defaults to Rust after migration.* The
  host emits `eosd` spawn/thin-client commands by default; selection no longer
  reads `EOS_SANDBOX_RUNTIME` for normal operation (the Python launcher lives
  only inside the GC-01 compat bridge). `DAEMON_PROTOCOL_VERSION` and
  `runtime_artifact::PROTOCOL_VERSION` are asserted equal at compile time.
- **GC-eos-sandbox-host-05** — *background upload overlap without a thread pool.*
  The Python `ThreadPoolExecutor` overlap becomes one structured `JoinSet` task
  launched before `ensure_git` and drained after, errors swallowed-by-design so
  the sequential bootstrap retries (`async-joinset-structured`).
- **GC-eos-sandbox-host-06** — *registry fallback never caches (WR-01).*
  `adapter(unknown_id)` returns the default without inserting into `bindings`;
  `has(unknown_id)` stays `false`; a property test guards against cache growth.
- **GC-eos-sandbox-host-07** — *typed context-preparer.* The duck-typed
  `context_preparer(...) -> Any` becomes the concrete `ContextPreparer` enum so
  static analysis has a fixed point per adapter (matches the protocol.py
  rationale). This adds no new trait seam beyond the §6 map (`ProviderAdapter`);
  the enum is concrete (`type-enum-states`).

## 11. Acceptance Criteria

TDD — each AC names a failing test to write first. Maps to anchor §11 row
`eos-sandbox-api/host` ("daemon envelope tests; Docker selection;
provisioning").

- **AC-eos-sandbox-host-01** — provider selection from config/env resolves to
  Docker, and any non-Docker value returns
  `SandboxHostError::UnknownProviderKind`. *Test:* `registry::tests::selects_provider_from_config`.
- **AC-eos-sandbox-host-02** — `ProviderRegistry`: `register`+`adapter` returns the
  bound adapter; `adapter(unknown)` returns the default and `has(unknown)` is
  still `false` after the call (WR-01 / GC-06). *Test:*
  `registry::tests::fallback_does_not_cache` (+ `proptest` on random id sequences).
- **AC-eos-sandbox-host-03** — envelope builder produces
  `{op, invocation_id, args.layer_stack_root}` JSON; over TCP with a token the
  `_eos_daemon_auth_token` field is added; `api.v1.cancel` mints a fresh
  invocation id. *Test:* `daemon_client::tests::envelope_shape_and_auth`.
- **AC-eos-sandbox-host-04** — recovery: a mock transport returning
  `CONNECT_FAILED` triggers spawn → `api.runtime.ready` → replay; a mutating op
  (`api.v1.write_file`) returning empty-response **fails closed** (no replay).
  *Test:* `daemon_client::tests::recovery_retry_and_fail_closed`.
- **AC-eos-sandbox-host-05** — daemon `error` (non-policy) decodes to
  `DaemonDispatch{kind,message,details}`; a handler-level policy result
  (`success=false`, non-empty `status`) is returned, not raised. *Test:*
  `daemon_client::tests::decode_error_vs_policy_result`.
- **AC-eos-sandbox-host-06** — eosd upload verifies sha256: matching digest skips
  re-upload; mismatch returns `ArtifactHashMismatch`; unknown arch returns
  `UnsupportedArchitecture`. *Test:* `runtime_artifact::tests::upload_verifies_and_skips`.
- **AC-eos-sandbox-host-07** — the Docker adapter uploads via `put_archive` (the
  fast path), and a `#[cfg(test)]` mock `ProviderAdapter` substitutes for
  daemon/lifecycle unit tests without a real Docker daemon (seam substitutability).
  *Test:* `docker::tests::put_archive_fast_path`.
- **AC-eos-sandbox-host-07b** — TCP endpoint resolution single-flights: concurrent
  callers for one sandbox perform one async resolve, no registry/cache guard is
  held across the await, and cache invalidation triggers a fresh single-flight.
  *Test:* `daemon_client::tests::tcp_endpoint_singleflight_lock_order`.
- **AC-eos-sandbox-host-08** — `DAEMON_PROTOCOL_VERSION == runtime_artifact::PROTOCOL_VERSION`
  (compile-time assert; build fails otherwise). *Test:* compile + `version_lockstep`.
- **AC-eos-sandbox-host-09** — `RequestSandboxProvisioner::prepare_for_run` starts
  an explicit sandbox id, or creates one labelled `origin=workflow,
  request_id=<id>` and errors when create returns no id. *Test:*
  `provisioning::tests::prepare_explicit_and_fresh`.
- **AC-eos-sandbox-host-10** — `ProviderAdapter` is sealed: an out-of-crate impl
  fails to compile (documented compile-fail / `trybuild`). *Test:*
  `tests/compile_fail/sealed_adapter.rs`.
- **AC-eos-sandbox-host-11** — isolated workspace enter/exit preserves the host
  lifecycle gates: enter rejects local or daemon in-flight ephemeral jobs and
  command sessions; exit cancels/drains per-agent background work, calls daemon
  exit, releases the namespace/snapshot lease, and clears scratch state. *Test:*
  `isolated_workspace::tests::enter_exit_lifecycle_gates`.

## 12. Implementation Checklist

1. `error.rs`: define `SandboxHostError`; wire `#[from]` for io/json (verify: `cargo check`).
2. `provider.rs`: `ProviderKind`, `CreateSandboxSpec`, `SandboxInfo`,
   `DaemonTcpEndpoint`, `ExecOpts`, `RawExecResult`, the sealed `ProviderAdapter`
   trait + the concrete `ContextPreparer` enum (verify: AC-10 compile-fail stub).
3. `registry.rs`: `ProviderRegistry` app state with `parking_lot::RwLock` fields; WR-01 no-cache
   fallback (verify: AC-01, AC-02).
4. `daemon_client.rs`: envelope builder + constants + decode (verify: AC-03, AC-05).
5. `daemon_client.rs`: spawn/connect/empty-response recovery state machine + TCP
   cache over a mock transport (verify: AC-04).
6. `runtime_artifact.rs`: eosd consts, arch map, sha verify, Docker `put_archive`
   upload, readiness, protocol lockstep assert (verify: AC-06, AC-08).
7. `docker.rs`: bollard adapter — CRUD, exec, `put_archive` fast path,
   `daemon_tcp_endpoint`, labels (verify: AC-07 + integration behind `docker`
   feature).
8. `#[cfg(test)]` mock `ProviderAdapter` for daemon/lifecycle unit tests.
9. `lifecycle.rs`: `SandboxLifecycle` create/start/stop/delete/set_labels/
   ensure_running + `setup_post_lifecycle` with JoinSet overlap (verify: GC-05 unit).
10. `isolated_workspace.rs`: enter/exit lifecycle gates and daemon op calls
   (verify: AC-11).
11. `provisioning.rs`: `RequestSandboxBinding` + provisioner (verify: AC-09).
12. `lib.rs`: re-exports; run `cargo fmt --check` + `clippy -D warnings`.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-sandbox-host` per spec-conventions.md §13. Do not edit other crates' rows.
