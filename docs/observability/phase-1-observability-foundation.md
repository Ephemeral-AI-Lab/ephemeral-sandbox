# Phase 1 Observability Foundation

Status: draft implementation spec

Parent spec: [sandbox-observability.md](./sandbox-observability.md)

## Exact Goal

Phase 1 creates only the local observability data model and SQLite store
foundation. It does not wire observability into daemon request handling, manager
aggregation, runtime operations, or live tracing.

The exact deliverable is:

- Add a minimal `sandbox-observability` crate to own SQLite code outside
  `sandbox-daemon` and `sandbox-runtime`.
- Define row-shaped records for the Phase 1 tables:
  - trace rows;
  - span rows;
  - sandbox snapshot rows.
- Derive the local observability directory from the daemon socket path.
- Create and migrate one local SQLite database:
  - `observability.sqlite`.
- Add focused tests for path derivation, schema idempotence, and basic store
  writes with synthetic records.

Phase 1 must not:

- Add production or test LOC under `crates/sandbox-runtime`.
- Add a `sandbox-observability` dependency to `sandbox-runtime`.
- Add SQLite imports, writer queues, observability paths, snapshot adapters, or
  trace context parameters to `sandbox-runtime`.
- Change `sandbox_protocol::Response`.
- Add `{ result, meta }` response envelopes.
- Add `DaemonObservabilityService` to `SandboxDaemonServer`.
- Add daemon observability RPC/API methods.
- Add manager aggregation or manager UI.
- Implement full sandbox, workspace, command, cgroup, disk, or resource
  sampling.
- Add method-span instrumentation across `exec_command`,
  `write_command_stdin`, `read_command_lines`, or `squash`.
- Add async command-finalization traces.
- Add Prometheus, Grafana, Loki, Tempo, OTLP, docker-compose observability
  config, or log export.
- Route command transcripts into observability storage.

## Current Repo Grounding

This section describes the live checkout, not the future design.

### Daemon Runtime State

`crates/sandbox-daemon/src/server/runtime.rs` currently defines `ServerConfig`
with:

- `socket_path`;
- `pid_path`;
- optional TCP fields;
- optional `auth_token`;
- optional `sandbox_id`.

`SandboxDaemonServer` currently stores only:

- `config: ServerConfig`;
- `operations: Arc<SandboxRuntimeOperations>`;
- `shutdown: CancellationToken`.

Phase 1 preserves that shape. It must not add an observability service field to
the daemon server.

### Daemon Startup

`crates/sandbox-daemon/src/serve.rs` parses `--sandbox-id`, stores it in
`ServerConfig`, and calls
`SandboxDaemonServer::new_with_runtime_config(server_config, runtime_config)`.

Phase 1 does not change daemon startup. Path derivation is tested in
`sandbox-observability` with synthetic socket paths. A later daemon integration
phase can decide how missing `sandbox_id` disables live observability.

### Sandbox Daemon Runtime Directory

The daemon must run inside the sandbox and use sandbox-internal `/eos` storage
for its runtime files and observability database:

```text
/eos/runtime/daemon/
  runtime.sock
  runtime.pid
  observability/
    observability.sqlite
```

The manager may keep host-side endpoint metadata, launch bookkeeping, or proxy
state so it can reach the daemon, but it must not create a manager-side
observability store. In particular, this design rejects a second store at:

```text
<manager_runtime_root>/<sandbox_id>/observability/observability.sqlite
```

Phase 1 derives from the daemon socket parent, and production daemon socket
paths must be under `/eos`:

```text
daemon_runtime_dir = socket_path.parent()
observability_dir = daemon_runtime_dir.join("observability")
database_path = observability_dir.join("observability.sqlite")
```

The production daemon config should therefore set:

```text
socket_path = /eos/runtime/daemon/runtime.sock
database_path = /eos/runtime/daemon/observability/observability.sqlite
```

It must not add a new manager-side observability path convention.

### Runtime Operation Graph

`crates/sandbox-runtime/operation/src/services.rs` currently owns the runtime
service graph:

- `SandboxRuntimeOperations`
  - `command: Arc<CommandOperationService>`;
  - `layerstack: Arc<LayerStackService>`.

`from_config` constructs:

- `WorkspaceRuntimeService`;
- `WorkspaceSessionService`;
- `LayerStackService`;
- `CommandOperationService`.

There is no observability service in `SandboxRuntimeOperations`. Phase 1 must
preserve that.

### Runtime Dispatch

`crates/sandbox-runtime/operation/src/operation.rs` currently dispatches with a
plain function pointer:

```rust
fn(&SandboxRuntimeOperations, &sandbox_protocol::Request)
    -> sandbox_protocol::Response
```

`dispatch_operation` finds an operation entry by `request.op` and returns a
plain `sandbox_protocol::Response`.

Phase 1 must not change this signature. Future method tracing can wrap this
boundary in Phase 3.

### Runtime State Owners

`crates/sandbox-runtime/operation/src/command/service/core.rs` owns command
state through `CommandOperationService` and its private `CommandProcessStore`.

`crates/sandbox-runtime/operation/src/workspace_session/service/core.rs` owns
workspace sessions through a private `sessions` map.

Phase 1 must not expose these internals, add snapshot methods, or create DTO
adapters for them. Runtime snapshot adapters belong to Phase 2.

### Existing SQLite Constraint

`crates/sandbox-daemon/tests/unit/dependency_guard.rs` currently asserts that
the daemon manifest does not contain `rusqlite` or `host`.

Phase 1 preserves that intent by keeping SQLite code in the new
`sandbox-observability` crate and not adding a daemon dependency on `rusqlite`.
The daemon dependency guard does not need to change unless a later phase wires
`sandbox-daemon` to `sandbox-observability`.

## Recommended Architecture

Create one minimal crate:

```text
crates/sandbox-observability/
```

This crate owns only:

- path derivation;
- Phase 1 row records;
- SQLite schema initialization;
- direct, synchronous store insert/upsert helpers used by tests and later
  daemon integration.

This crate does not own:

- `DaemonObservabilityService`;
- a bounded writer queue;
- a disabled/no-op writer;
- snapshot samplers;
- trace context;
- daemon RPC/API types;
- manager aggregation types;
- runtime adapters.

The crate is justified only by the current daemon dependency guard against
direct `rusqlite`. Without that guard, the same code would be small enough to
live as daemon-private modules until a second consumer exists.

## Redundancy Decisions

Keep these Phase 1 pieces:

- One `sandbox-observability` crate as the SQLite dependency boundary.
- One SQLite database, `observability.sqlite`.
- One concrete store type, `ObservabilityStore`.
- Row-shaped records that match the Phase 1 schema.
- Path derivation from `ServerConfig.socket_path.parent()`.
- Focused crate tests.

Delete or defer these pieces:

- No `DaemonObservabilityService` in Phase 1.
- No daemon `observability` field.
- No daemon observability API/RPC module.
- No manager-side observability files.
- No `samplers/` folder.
- No `trace_context.rs` or `OperationTrace`.
- No `ObservabilitySink` trait.
- No `NullObservabilityWriter`.
- No bounded writer queue, worker thread, or Tokio task.
- No separate `method-trace.sqlite` and `sandbox-state.sqlite`.
- No `trace_links` table.
- No async trace correlation fields.
- No workspace, command, resource, cgroup, or disk snapshot tables.
- No writer statistics or retention-policy tables.

## Sandbox-Runtime Impact Budget

Phase 1 must add exactly `0` production LOC and `0` test LOC under:

```text
crates/sandbox-runtime/
```

That means Phase 1 must not add:

- a `sandbox-observability` dependency to
  `crates/sandbox-runtime/operation/Cargo.toml`;
- a top-level runtime `observability` module;
- SQLite imports;
- writer handles;
- `OperationTrace`;
- dispatch signature changes;
- command/workspace snapshot methods;
- runtime tests for observability.

Future phases may require runtime read adapters, but those should stay narrow
and colocated with the state owner:

```text
crates/sandbox-runtime/operation/src/command/service/snapshot.rs
crates/sandbox-runtime/operation/src/workspace_session/service/snapshot.rs
```

Those files are not Phase 1 files.

## Resulting File and Folder Structure

### Workspace

```text
Cargo.toml
```

Phase 1 changes:

- Add `crates/sandbox-observability` to `workspace.members`.
- Add `sandbox-observability = { path = "crates/sandbox-observability" }` to
  `workspace.dependencies` only if another crate needs the dependency in the
  same patch. Otherwise the crate can stay a workspace member without a shared
  workspace dependency entry.
- Reuse existing `rusqlite = { version = "0.32", features = ["bundled"] }`.

### New Crate: `sandbox-observability`

```text
crates/sandbox-observability/
  Cargo.toml
  src/
    lib.rs
    paths.rs
    records.rs
    store.rs
  tests/
    paths.rs
    schema.rs
```

Responsibilities:

- `lib.rs`
  - exports `ObservabilityPaths`, `ObservabilityStore`, and Phase 1 record
    types.

- `paths.rs`
  - defines `ObservabilityPaths`;
  - derives `daemon_runtime_dir` from `socket_path.parent()`;
  - derives `observability_dir`;
  - derives `database_path`;
  - treats `/eos/runtime/daemon/runtime.sock` as the production socket-path
    shape;
  - does not create directories.

- `records.rs`
  - defines `TraceRecord`;
  - defines `SpanRecord`;
  - defines `SandboxSnapshotRecord`;
  - bounds string fields before insertion.

- `store.rs`
  - opens `observability.sqlite`;
  - creates the observability directory;
  - applies v1 schema idempotently;
  - inserts synthetic trace/span records;
  - upserts synthetic sandbox snapshot records;
  - contains migration SQL locally instead of a `store/migrations.rs` tree.

- `tests/paths.rs`
  - verifies socket-path derivation.

- `tests/schema.rs`
  - verifies schema idempotence;
  - verifies basic trace/span insert;
  - verifies sandbox snapshot upsert.

### Daemon Crate

```text
crates/sandbox-daemon/
```

Phase 1 changes:

- Add `0` production LOC.
- Add `0` server fields.
- Add `0` daemon observability APIs.
- Keep the daemon dependency guard intent: no direct `rusqlite`.

### Runtime Crate

```text
crates/sandbox-runtime/
```

Phase 1 changes:

- Add `0` production LOC.
- Add `0` test LOC.
- Add `0` files.
- No SQLite dependency.
- No `sandbox-observability` dependency.
- No dispatch signature change.
- No `OperationTrace` parameter.
- No operation instrumentation.
- No snapshot adapter methods.

### Manager Crate

```text
crates/sandbox-manager/
```

Phase 1 changes:

- Add `0` production LOC.
- Add `0` manager aggregation.
- Add `0` daemon polling API.
- Add `0` UI/display changes.

## SQLite Schema and Migration Plan

The Phase 1 database is:

```text
/eos/runtime/daemon/observability/observability.sqlite
```

Recommended pragmas:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 1000;
PRAGMA foreign_keys = ON;
```

Schema:

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  checksum TEXT NOT NULL,
  applied_at_unix_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS traces (
  trace_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  sandbox_id TEXT NOT NULL,
  operation TEXT NOT NULL,
  request_id TEXT,
  started_at_unix_ms INTEGER NOT NULL,
  finished_at_unix_ms INTEGER,
  duration_ms REAL,
  error_kind TEXT,
  error_message TEXT
);

CREATE TABLE IF NOT EXISTS spans (
  span_id TEXT PRIMARY KEY,
  trace_id TEXT NOT NULL,
  parent_span_id TEXT,
  method_name TEXT NOT NULL,
  call_index INTEGER NOT NULL,
  status TEXT NOT NULL,
  started_at_unix_ms INTEGER NOT NULL,
  finished_at_unix_ms INTEGER,
  duration_ms REAL,
  error_kind TEXT,
  error_message TEXT,
  FOREIGN KEY(trace_id) REFERENCES traces(trace_id) ON DELETE CASCADE,
  FOREIGN KEY(parent_span_id) REFERENCES spans(span_id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS sandbox_snapshots (
  sandbox_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  sampled_at_unix_ms INTEGER NOT NULL,
  error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_traces_request
  ON traces(request_id);

CREATE INDEX IF NOT EXISTS idx_traces_sandbox_started
  ON traces(sandbox_id, started_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_spans_trace_call_index
  ON spans(trace_id, call_index);
```

Migration rules:

- Apply migrations inside a transaction.
- Use a fixed ordered list of migrations in code.
- Insert one row in `schema_migrations` per applied migration.
- Running initialization twice must be a no-op.
- Do not add a retention-policy table in Phase 1.
- Do not add `PRAGMA user_version` unless it reduces implementation code.

Deferred schema:

- `sandbox_snapshots` is only the Phase 1 seed/current-status table for the
  sandbox root. It is not sufficient for the full observability hierarchy.
- `workspace_snapshots` must be added alongside `sandbox_snapshots` in Phase 2
  once runtime snapshot adapters expose active workspace/session state. This
  table owns the `sandbox -> workspace` hierarchy.
- `execution_snapshots` must be added in Phase 2 once runtime execution snapshot
  adapters expose active/recent execution state. This table owns active runtime
  execution display under each workspace.
- `resource_samples` must be added in Phase 2 once daemon-side resource
  samplers exist. This table owns sandbox-global samples
  (`workspace_id IS NULL`) and per-workspace samples (`workspace_id IS NOT NULL`).
- `trace_links`, `correlation_kind`, `correlation_id`, `origin_request_id`, and
  `async_name` wait for Phase 4 async traces.
- Extra workspace/command/correlation indexes wait for the query paths that use
  them.

The full target hierarchy is therefore backed by these table roles:

```text
sandbox_snapshots       -> sandbox root state
workspace_snapshots     -> workspace rows under each sandbox
resource_samples        -> sandbox-global and per-workspace resource history
execution_snapshots     -> active/recent runtime executions under each workspace
traces + spans          -> recent request/method chains
trace_links             -> later async relationships back to requests/commands
```

Implementation phase mapping:

```text
Phase 1
  sandbox_snapshots       -> schema + synthetic store upsert only
  traces + spans          -> schema + synthetic insert only

Phase 2
  sandbox_snapshots       -> live daemon sandbox-root snapshot population
  workspace_snapshots     -> live workspace rows under each sandbox
  execution_snapshots     -> live active/recent execution rows
  resource_samples        -> live sandbox-global and per-workspace resource samples

Phase 3
  traces + spans          -> live request/method-chain tracing from operations

Phase 4
  trace_links             -> async relationships back to requests/commands
```

Phase 1 therefore establishes storage shape for `sandbox_snapshots`, `traces`,
and `spans`, but it does not make any of them live observability producers.

## Store Semantics

`ObservabilityStore` is direct and synchronous in Phase 1.

Expected API shape:

```rust
pub struct ObservabilityStore { /* private */ }

impl ObservabilityStore {
    pub fn open(paths: &ObservabilityPaths) -> Result<Self, StoreError>;
    pub fn insert_trace(&self, trace: &TraceRecord, spans: &[SpanRecord])
        -> Result<(), StoreError>;
    pub fn upsert_sandbox_snapshot(&self, snapshot: &SandboxSnapshotRecord)
        -> Result<(), StoreError>;
}
```

Phase 1 does not need a nonblocking call surface because no live daemon/runtime
hot path writes observability records yet. A bounded queue should be added in
the first later phase that introduces a live hot producer.

## Phase 1 Boundaries

### In Scope

- Minimal observability crate.
- Path derivation.
- One SQLite database.
- Idempotent schema migration.
- Row-shaped records.
- Direct store insert/upsert helpers.
- Crate tests for the store foundation.
- Documentation that preserves the `0` LOC sandbox-runtime rule.

### Out of Scope

- Daemon service wiring.
- Daemon observability APIs.
- Full sandbox snapshot collection.
- Workspace-session snapshot methods.
- Command-process snapshot methods.
- Cgroup implementation.
- Disk upperdir scanning.
- Method-span instrumentation for live operations.
- Async command finalization traces.
- Writer queue pressure handling.
- Manager aggregation.
- Manager UI.
- Public protocol response changes.
- Prometheus, Grafana, Loki, Tempo, OTLP, or log export.

## Expected LOC Change

Rough production-code estimate:

```text
Cargo.toml workspace additions                         3-8
crates/sandbox-observability/Cargo.toml               18-30
crates/sandbox-observability/src/lib.rs               15-25
crates/sandbox-observability/src/paths.rs             35-60
crates/sandbox-observability/src/records.rs           50-90
crates/sandbox-observability/src/store.rs            150-230
crates/sandbox-daemon/**                               0
crates/sandbox-runtime/**                              0
crates/sandbox-manager/**                              0
```

Expected production additions: about 271-443 LOC.

Rough test estimate:

```text
crates/sandbox-observability/tests/paths.rs            35-60
crates/sandbox-observability/tests/schema.rs          120-200
crates/sandbox-daemon/**                                0
crates/sandbox-runtime/**                               0
crates/sandbox-manager/**                               0
```

Expected test additions: about 155-260 LOC.

Expected code additions excluding docs: about 426-703 LOC.

The `sandbox-runtime` share of those Phase 1 additions is exactly `0`
production LOC and `0` test LOC.

## Verification Plan

Run formatting and package-level checks:

```sh
cargo fmt --check
cargo check -p sandbox-observability --tests
```

Run focused tests:

```sh
cargo test -p sandbox-observability
```

Optional guard check if the implementation touches daemon manifests:

```sh
cargo test -p sandbox-daemon dependency_guard
```

Do not add sandbox-runtime observability tests in Phase 1. A `sandbox-runtime`
check is only needed if the workspace manifest change unexpectedly affects
runtime compilation.

## Phase 1 Completion Criteria

Implementers must update this checklist in the spec before claiming Phase 1 is
complete. A Phase 1 implementation is not complete while any required checkbox
below remains unchecked.

Storage shape:

- [x] `crates/sandbox-observability` is a workspace member.
- [x] `observability.sqlite` is the only Phase 1 observability database.
- [x] No separate `method-trace.sqlite` or `sandbox-state.sqlite` file is
  created in Phase 1.
- [x] `schema_migrations`, `traces`, `spans`, and `sandbox_snapshots` are the
  only Phase 1 tables.
- [x] `workspace_snapshots`, `execution_snapshots`, `resource_samples`, and
  `trace_links` are not created in Phase 1.

Path and store behavior:

- [x] `ObservabilityPaths` derives the database path from the parent of the
  daemon socket path.
- [x] Production daemon socket paths use sandbox-internal `/eos` storage,
  yielding `/eos/runtime/daemon/observability/observability.sqlite`.
- [x] `ObservabilityPaths` does not introduce a new manager-side observability
  path convention.
- [x] `ObservabilityStore::open` creates the observability directory and
  initializes the schema.
- [x] Store initialization can run repeatedly without changing behavior.
- [x] Synthetic trace/span records can be inserted.
- [x] A synthetic sandbox snapshot can be upserted.

Phase boundary:

- [x] `sandbox_protocol::Response` remains unchanged.
- [x] `crates/sandbox-runtime` has `0` production LOC and `0` test LOC added.
- [x] `crates/sandbox-runtime` does not depend on `sandbox-observability`.
- [x] `SandboxDaemonServer` does not gain a `DaemonObservabilityService` field.
- [x] No daemon observability RPC/API is added.
- [x] No manager aggregation, daemon polling, or UI/display changes are added.
- [x] No live workspace, command, cgroup, disk, or resource sampler is added.
- [x] No live method-span instrumentation or async command-finalization tracing
  is added.
- [x] No bounded writer queue, writer worker, or disabled/no-op writer
  abstraction is added.
- [x] No Prometheus/Grafana/Loki/Tempo/OTLP files, dependencies, config, or
  runtime paths are added.

Verification:

- [x] `cargo fmt --check` passes.
- [x] `cargo check -p sandbox-observability --tests` passes.
- [x] `cargo test -p sandbox-observability` passes.
- [x] If daemon manifests are touched, the daemon dependency guard test passes.
- [x] The final implementation notes explicitly confirm that Phase 1 creates
  storage shape only and does not create live observability producers.
