# Phase 5: Manager Observability Aggregation

Phase 5 adds bounded query APIs over the local observability state already
written by earlier phases:

- daemon API `get_observability_snapshot`;
- manager API `get_observability_tree`;
- manager fan-out across ready sandbox daemons;
- typed snapshot DTO projection;
- bounded partial-failure handling;
- bounded resource and recent-trace query options.

This phase is a pull model. It does not make observability a control-plane
dependency, a manager-owned database, a raw SQL access surface, or a global
telemetry pipeline.

The authoritative observability store remains daemon-local:

```text
/eos/runtime/daemon/observability/observability.sqlite
```

The manager may keep endpoint, proxy, auth, and lifecycle metadata so it can
reach each daemon. The manager must not own, open, copy, mirror, compact,
migrate, or read daemon SQLite files.

## Non-Goals

Phase 5 must not implement:

- new runtime snapshot producers;
- new runtime tracing spans;
- SQLite reads or writes from `sandbox-runtime`;
- a `sandbox-observability` dependency from `sandbox-runtime`;
- manager-side `observability.sqlite`;
- manager-side mirror tables or cache databases;
- raw SQL query APIs;
- `get_method_trace`, `list_method_traces`, or `get_resource_samples`
  drilldown APIs;
- Prometheus, Grafana, Loki, Tempo, OTLP, or log export;
- command transcript or command output ingestion;
- response envelopes such as `{ result, meta }`;
- public command response-shape changes;
- a global event bus or streaming telemetry protocol.

## Current Repo Grounding

Docs are proposals. The live checkout is the source of truth for implementation
shape.

The intended production daemon runs inside the sandbox. Its daemon runtime
directory is:

```text
/eos/runtime/daemon/
  runtime.sock
  runtime.pid
  observability/
    observability.sqlite
```

`crates/sandbox-observability/src/paths.rs` currently derives the observability
database from `ServerConfig.socket_path.parent()` through
`ObservabilityPaths::from_socket_path`:

```text
daemon_runtime_dir = socket_path.parent()
observability_dir = daemon_runtime_dir.join("observability")
database_path = observability_dir.join("observability.sqlite")
```

With production `socket_path = /eos/runtime/daemon/runtime.sock`, the database
path is `/eos/runtime/daemon/observability/observability.sqlite`. Tests use temp
socket roots, and `LocalSandboxDaemonInstaller` currently models host endpoint
metadata as `runtime_root/<sandbox_id>/runtime.sock`. Phase 5 must not infer a
manager-side observability root from that endpoint metadata.

`crates/sandbox-daemon/src/server/runtime.rs` defines `ServerConfig` with
`socket_path`, `pid_path`, optional TCP fields, optional `auth_token`, and
optional `sandbox_id`. `SandboxDaemonServer::new_with_runtime_config` creates
`DaemonObservability::from_config(&config).map(Arc::new)` and installs the
async trace sink into `SandboxRuntimeOperations::from_config_with_async_trace_sink`.

`crates/sandbox-daemon/src/observability/service.rs` defines
`DaemonObservability::from_config`. It enables observability only when:

- `ServerConfig.sandbox_id` is present and non-empty;
- `ObservabilityPaths::from_socket_path(config.socket_path.clone())` succeeds;
- `ObservabilityStore::open(&paths)` succeeds.

If any of those checks fail, it returns `None` and daemon request handling still
continues. The enabled service stores the bounded `sandbox_id`, `paths`, the
`ObservabilityStore`, disk-sample cache state, and deep-span enablement state.

`SandboxDaemonServer::trigger_observability_collection` currently clones
`observability`, `config`, and `operations`, then starts a detached
`tokio::task::spawn_blocking` call that runs:

```rust
let _ = observability.collect(&config, &operations);
```

The result is intentionally ignored and the handle is dropped. Snapshot writes
are daemon-local and best effort.

`DaemonObservability::collect` currently samples
`operations.observability_snapshot()`, writes sandbox, workspace, namespace
execution, and resource rows through `write_snapshot`, then acknowledges only
successfully projected completed namespace execution ids with
`operations.ack_completed_namespace_executions`. Phase 5 assumes the Phase 4.6
hard cutover has removed the older command-shaped execution snapshot lane.

`crates/sandbox-observability/src/store.rs` currently has production write APIs:

- `ObservabilityStore::open`;
- `insert_trace`;
- `upsert_sandbox_snapshot`;
- `upsert_workspace_snapshots`;
- `reconcile_workspace_snapshots`;
- `upsert_namespace_execution_snapshots`;
- `prune_namespace_execution_snapshots`;
- `insert_namespace_execution_trace`;
- `insert_resource_samples`.

It currently has no production read APIs. Existing reads are test-support only:

- `trace_for_test`;
- `spans_for_test`;
- `sandbox_snapshot_for_test`;
- `workspace_snapshots_for_test`;
- `namespace_execution_snapshots_for_test`;
- `namespace_execution_traces_for_test`;
- `resource_samples_for_test`;
- `force_sqlite_write_errors_for_test`.

There is no current production or test helper that lists recent trace summaries.
Tests read ordinary request/async traces by trace id and spans by trace id, and
read namespace execution traces by sandbox id.

Phase 5 needs production read helpers that return bounded row records or typed
projection inputs. It must not expose a generic SQL helper or public raw
`rusqlite::Connection`.

The current schema has these query-relevant indexes:

- `idx_traces_request` on `traces(request_id)`;
- `idx_traces_sandbox_started` on `traces(sandbox_id, started_at_unix_ms)`;
- `idx_spans_trace_call_index` on `spans(trace_id, call_index)`;
- `idx_workspace_snapshots_sandbox` on
  `workspace_snapshots(sandbox_id, workspace_id)`;
- `idx_resource_samples_workspace_time` on
  `resource_samples(sandbox_id, workspace_id, sampled_at_unix_ms)`;
- `idx_resource_samples_sandbox_time` on
  `resource_samples(sandbox_id, sampled_at_unix_ms)`;
- `idx_namespace_execution_snapshots_workspace_session` on
  `namespace_execution_snapshots(sandbox_id, workspace_session_id)`;
- `idx_namespace_execution_traces_namespace_execution` on
  `namespace_execution_traces(sandbox_id, namespace_execution_id)`;
- `idx_namespace_execution_traces_workspace_session_started` on
  `namespace_execution_traces(sandbox_id, workspace_session_id,
  started_at_unix_ms)`.

`crates/sandbox-protocol/src/request.rs` defines the current request shape:

```rust
pub struct Request {
    pub op: String,
    pub request_id: String,
    pub scope: CliOperationScope,
    pub args: Value,
}
```

`crates/sandbox-protocol/src/scope.rs` defines
`CliOperationScope::System` and `CliOperationScope::Sandbox { sandbox_id }`.
Sandbox ids must be non-empty.

`crates/sandbox-protocol/src/response.rs` wraps a private
`serde_json::Value`. `Response::ok(result)` and `Response::running(result)`
return the result directly. Faults are top-level objects with an `error` field.
Phase 5 must not add a `{ "result": ..., "meta": ... }` envelope.

`crates/sandbox-manager/src/router/dispatch.rs` currently decides manager-owned
operations by checking `crate::cli_operation_specs()` for `request.op`:

- system-scoped manager-owned requests run locally in
  `dispatch_manager_request`;
- system-scoped non-manager requests return `Response::unknown_op()`;
- sandbox-scoped manager-owned requests return an invalid-request fault;
- sandbox-scoped non-manager requests go to `forward_sandbox_request`.

`crates/sandbox-manager/src/router/forward.rs` resolves the sandbox id from
`CliOperationScope::Sandbox`, inspects the store, requires
`SandboxState::Ready`, requires a daemon endpoint, then calls
`services.daemon_client.invoke(&endpoint, request)`.

`crates/sandbox-manager/src/daemon_client.rs` currently models daemon transport
as the `SandboxDaemonClient` trait with `invoke(&SandboxDaemonEndpoint, Request)
-> Response`.

`SandboxDaemonEndpoint` in `crates/sandbox-manager/src/model.rs` currently holds
`socket_path` and optional `auth_token`. Response DTOs must never echo the auth
token.

`SandboxStore` in `crates/sandbox-manager/src/store.rs` stores records in a
mutex-protected `HashMap`. `list()` returns records sorted by `SandboxId`;
`inspect()` returns one record or `ManagerError::MissingSandbox`. There is no
ready-only query helper today. Phase 5 should select ready records by calling
`list()` and filtering `record.state == SandboxState::Ready`.

Manager operations are registered in
`crates/sandbox-manager/src/operation/dispatch.rs` as `ManagerOperationEntry`
values. The management family in
`crates/sandbox-manager/src/operation/impls/management/mod.rs` currently
registers `create_sandbox`, `destroy_sandbox`, `list_sandboxes`, and
`inspect_sandbox`.

The daemon currently has no daemon-owned operation registry. In
`crates/sandbox-daemon/src/server/dispatch.rs`, `dispatch_request` validates
daemon scope through `validate_daemon_scope(&request)`, creates optional runtime
trace state, and calls:

```rust
sandbox_runtime::dispatch_operation(&operations, &request, trace.as_ref())
```

inside `tokio::task::spawn_blocking`. Phase 5 should add the daemon-owned
observability branch before this runtime dispatch.

Current test coverage relevant to Phase 5:

- `crates/sandbox-daemon/tests/unit/observability.rs` covers daemon snapshot
  writes, disk sample caching, disabled observability, request trace persistence,
  async trace persistence, namespace execution projection, and write failures
  not changing operation responses.
- `crates/sandbox-manager/tests/manager_router.rs` covers local manager
  dispatch, manager scope rejection, sandbox request forwarding, missing
  sandboxes, and daemon-unavailable forwarding failures.
- `crates/sandbox-manager/tests/manager_core.rs` covers manager catalog
  contents, create/list/inspect/destroy behavior, daemon installer behavior, and
  store duplicate/missing errors.
- `crates/sandbox-observability/tests/schema.rs` covers schema migration,
  trace/span insert, async trace fields, sandbox/workspace/resource rows,
  namespace execution rows, and allowed indexes.

## Self-Critical Architecture Check

### Load and Safety

`get_observability_tree` remains summary-first. It returns one bounded
`SandboxSnapshot` per selected sandbox, with current state, latest resources by
default, active workspace and namespace execution summaries, and optional recent
trace summaries. It does not return raw spans, full resource history,
transcripts, command output, SQL rows, or file lists.

The manager issues at most `8` daemon calls concurrently. This is a constant,
`MAX_OBSERVABILITY_TREE_FANOUT`, not caller-controlled input.

Each daemon call has a `1500 ms` timeout, `PER_DAEMON_OBSERVABILITY_TIMEOUT`.
Because the current `SandboxDaemonClient::invoke` trait is synchronous and has
no timeout parameter, Phase 5 should add the smallest timeout-aware helper:

```rust
fn invoke_with_timeout(
    &self,
    endpoint: &SandboxDaemonEndpoint,
    request: sandbox_protocol::Request,
    timeout: std::time::Duration,
) -> Result<sandbox_protocol::Response, ManagerError>;
```

The production client must enforce the timeout at the transport layer. Existing
forwarding can keep using `invoke`; `get_observability_tree` must use the
timeout-aware helper.

If one daemon is slow, unavailable, unauthorized, returns a transport error, or
returns malformed data, the manager maps that sandbox to an unavailable node.
It does not fail the whole tree unless the manager cannot parse its own request
or cannot read the manager store.

The manager can return partial results. A failed daemon becomes a
`SandboxSnapshot` with `availability = "unavailable"`, a bounded reason, no
resources, no workspaces, and no leaked endpoint auth token.

The daemon caps caller input regardless of requested values:

- default `trace_limit`: `10`;
- maximum `trace_limit`: `50`;
- default `resource_window_ms`: `None`, meaning latest resource sample only;
- maximum `resource_window_ms`: `300_000`;
- maximum resource history samples per scope: `128`.

When `include_resources = true` and `resource_window_ms = None`, the daemon
returns latest/current resources only. It does not return resource history by
default.

The daemon must reuse cached disk samples. A query may trigger a rate-limited
snapshot collection, but it must call the existing collection path with
`force_fresh_disk = false` and honor the existing disk cache interval. A query
must not force a fresh expensive disk walk on every request.

SQLite lock or read failures return bounded partial snapshot errors when live
runtime state can still be sampled. They must not fail unrelated user
operations and must not make the manager open SQLite directly.

### Ownership Boundaries

The daemon remains responsible for `/eos` storage reads, current runtime
snapshot collection, trace/resource projection, and local partial errors.

The manager remains responsible only for selecting sandbox records, contacting
daemon endpoints, applying bounded fan-out, and aggregating typed DTOs.

The manager avoids direct filesystem access to
`/eos/runtime/daemon/observability/observability.sqlite`.

The manager avoids a second observability cache database, mirror table, or
compaction path.

`sandbox-runtime` stays unchanged in production code. It already exposes the
runtime snapshot producers needed by the daemon. Phase 5 must not add runtime
snapshot producers, runtime spans, SQLite reads/writes, or a
`sandbox-observability` dependency to runtime crates.

Phase 5 should not expose `sandbox-observability` row types in manager public
responses. Row records remain storage implementation details. Stable API DTOs
belong at the daemon/manager projection layer.

### API Minimality

Phase 5 reduces to one daemon query and one manager aggregation query:

- `get_observability_snapshot`;
- `get_observability_tree`.

Do not add `get_method_trace`, `list_method_traces`, or
`get_resource_samples` drilldown APIs in this phase.

`get_observability_snapshot` should be a daemon-owned dispatch branch, not a
runtime operation. It reads daemon-local observability state and is allowed to
sample current runtime state only through the existing
`SandboxRuntimeOperations::observability_snapshot()` method. Adding it to
`sandbox-runtime` would violate the Phase 5 zero-runtime-LOC rule.

The current catalog enum has manager and runtime execution spaces. Phase 5 does
not need to make the daemon query part of the runtime catalog. The smallest
catalog/help change is:

- add a daemon execution-space name to `sandbox-protocol` catalog/help support;
- define a small daemon observability operation spec in `sandbox-daemon`;
- expose a daemon catalog for tooling that asks the daemon for daemon-owned
  operations;
- keep the request itself sandbox-scoped and direct over the existing
  `Request`/`Response` protocol.

This avoids changing `sandbox-runtime::cli_operation_catalog()` and avoids a
runtime crate dependency on daemon observability.

If adding a full daemon operation registry proves unnecessary, the first
implementation can use one constant op-name branch in daemon dispatch plus one
daemon catalog spec for help. Do not build a broad daemon operation framework
until a second daemon-owned operation exists.

Manager aggregation should call the daemon through `SandboxDaemonClient`, not
through filesystem or SQLite APIs. It should use the timeout-aware helper above.
No narrower helper is needed unless the concrete transport cannot enforce the
timeout through `SandboxDaemonClient`.

Required hierarchy fields are current state, latest resources, active workspace
rows, active namespace execution summaries, recent trace summaries, and bounded
partial errors. Full spans, raw resource history, transcript content, command
output, environment data, stdin, stdout/stderr chunks, raw SQL columns, and file
lists are deferred or forbidden.

The simpler daemon-owned branch plus manager fan-out design works, so Phase 5
uses it. It rejects manager-side SQLite reads, runtime operation ownership,
query drilldowns, and telemetry-pipeline infrastructure.

## Daemon Query API

Daemon-owned operation:

```text
op = "get_observability_snapshot"
scope = CliOperationScope::Sandbox { sandbox_id }
execution space = sandbox daemon
```

Input:

```rust
pub struct GetObservabilitySnapshotInput {
    pub workspace_id: Option<String>,
    pub include_resources: bool,
    pub include_recent_traces: bool,
    pub resource_window_ms: Option<u64>,
    pub trace_limit: Option<u32>,
}
```

Output:

```rust
pub struct GetObservabilitySnapshotOutput {
    pub sandbox: SandboxSnapshot,
}
```

JSON arguments:

```json
{
  "workspace_id": "workspace-1",
  "include_resources": true,
  "include_recent_traces": false,
  "resource_window_ms": 60000,
  "trace_limit": 10
}
```

All fields are optional at the JSON boundary. Defaults:

```text
workspace_id = None
include_resources = true
include_recent_traces = false
resource_window_ms = None
trace_limit = Some(10) when include_recent_traces is true
```

Daemon caps:

```text
MAX_TRACE_LIMIT = 50
MAX_RESOURCE_WINDOW_MS = 300_000
MAX_RESOURCE_HISTORY_SAMPLES_PER_SCOPE = 128
MIN_QUERY_COLLECTION_INTERVAL_MS = 500
```

The daemon clamps excessive `trace_limit` and `resource_window_ms` before
reading SQLite. It does not return an invalid-request fault for over-large
limits unless the value cannot be parsed as an unsigned integer.

`workspace_id = None` returns sandbox-global state plus all workspace summaries.
`workspace_id = Some(..)` returns sandbox-global state plus only that workspace
when it exists. If the workspace id is syntactically empty, return
`invalid_request`. If it is valid but unknown, return the sandbox snapshot with
`workspaces = []` and a bounded partial error:

```json
{
  "kind": "workspace_not_found",
  "message": "workspace not found"
}
```

When observability is disabled because `DaemonObservability::from_config`
returned `None`, the daemon returns `Response::ok` with an unavailable sandbox
node. It does not return a top-level fault:

```json
{
  "sandbox": {
    "sandbox_id": "sandbox-1",
    "state": "unavailable",
    "availability": "unavailable",
    "sampled_at_unix_ms": null,
    "resources": null,
    "workspaces": [],
    "partial_errors": [
      {
        "kind": "observability_disabled",
        "message": "observability is disabled for this daemon"
      }
    ]
  }
}
```

When SQLite read fails but live runtime state can still be sampled, the daemon
should:

1. Sample `operations.observability_snapshot()`.
2. Project live sandbox/workspace/execution/namespace execution DTO fields from
   memory.
3. Omit trace summaries and resource history that require the store.
4. Return `availability = "partial"` with a bounded `sqlite_read_failed` partial
   error.

When both SQLite reads and live sampling fail, return an unavailable sandbox
node inside `Response::ok`.

The query should attempt rate-limited current collection before reading rows.
Rules:

- if the last query-triggered collection was more than
  `MIN_QUERY_COLLECTION_INTERVAL_MS` ago, call the existing collection path;
- do not force fresh disk sampling;
- ignore collection write failures except for a bounded partial error;
- read current rows after the collection attempt;
- if the rate limit suppresses collection, read the current rows already in the
  store.

Bounded errors appear only in the DTO `partial_errors` array. The protocol
payload remains the direct `Response::ok(result)` value.

Command transcript content remains excluded. The DTO must not include transcript
rows, stdout/stderr chunks, command output, stdin, environment variables, or
unbounded shell text. The first API should also omit `transcript_path`; callers
that need command output should use command APIs, not observability.

## Manager Query API

Manager-owned operation:

```text
op = "get_observability_tree"
scope = CliOperationScope::System
execution space = manager
```

Input:

```rust
pub struct GetObservabilityTreeInput {
    pub sandbox_ids: Option<Vec<String>>,
    pub include_resources: bool,
    pub include_recent_traces: bool,
    pub resource_window_ms: Option<u64>,
    pub trace_limit: Option<u32>,
}
```

Output:

```rust
pub struct GetObservabilityTreeOutput {
    pub sandboxes: Vec<SandboxSnapshot>,
}
```

JSON arguments:

```json
{
  "sandbox_ids": ["sandbox-1", "sandbox-2"],
  "include_resources": true,
  "include_recent_traces": false,
  "resource_window_ms": 60000,
  "trace_limit": 10
}
```

All fields are optional at the JSON boundary. Defaults match the daemon query:

```text
sandbox_ids = None
include_resources = true
include_recent_traces = false
resource_window_ms = None
trace_limit = Some(10) when include_recent_traces is true
```

`sandbox_ids = None` selects all manager records where:

```text
record.state == SandboxState::Ready
record.daemon.is_some()
```

Use `SandboxStore::list()` and keep its sorted id ordering.

`sandbox_ids = Some(..)` rules:

- reject syntactically invalid or empty ids with top-level `invalid_request`;
- de-duplicate while preserving first occurrence;
- for missing ids, return unavailable nodes with `state = "missing"`;
- for stopped, stopping, creating, or failed records, return unavailable nodes
  with their current manager state;
- for ready records without a daemon endpoint, return unavailable nodes with
  `reason.kind = "daemon_unavailable"`;
- for ready records with endpoints, query the daemon.

The manager constructs one daemon request per ready endpoint:

```rust
Request::new(
    "get_observability_snapshot",
    child_request_id(parent_request_id, sandbox_id),
    CliOperationScope::sandbox(sandbox_id.as_str()),
    json!({
        "workspace_id": null,
        "include_resources": input.include_resources,
        "include_recent_traces": input.include_recent_traces,
        "resource_window_ms": capped_resource_window_ms,
        "trace_limit": capped_trace_limit,
    }),
)
```

The manager must not include endpoint auth tokens in request args, responses, or
partial errors. Transport auth stays inside `SandboxDaemonEndpoint` and the
daemon client implementation.

Manager fan-out policy:

```text
MAX_OBSERVABILITY_TREE_FANOUT = 8
PER_DAEMON_OBSERVABILITY_TIMEOUT = 1500 ms
```

The first implementation should stay synchronous inside the existing manager
`spawn_blocking` dispatch model. Use bounded worker fan-out over the synchronous
daemon client. Do not change manager operation dispatch to async unless the
concrete daemon transport already has async timeout support and the diff is
smaller than a bounded synchronous helper.

Output ordering is deterministic:

- `sandbox_ids = None`: sorted by `SandboxStore::list()` order;
- `sandbox_ids = Some(..)`: first occurrence order after de-duplication.

Unavailable sandbox node shape:

```json
{
  "sandbox_id": "sandbox-1",
  "state": "ready",
  "availability": "unavailable",
  "sampled_at_unix_ms": null,
  "resources": null,
  "workspaces": [],
  "partial_errors": [
    {
      "kind": "daemon_timeout",
      "message": "daemon observability request timed out"
    }
  ]
}
```

If every selected daemon fails, still return `Response::ok` with unavailable
nodes for each selected sandbox. Use a top-level fault only when the manager
request itself is invalid or the manager store cannot be read.

Auth and endpoint errors are represented with bounded, non-secret kinds such as:

```text
daemon_unavailable
daemon_timeout
daemon_unauthorized
daemon_malformed_response
daemon_transport_failed
```

Do not include auth tokens, raw socket payloads, or unbounded transport error
strings in DTOs.

## DTO Shape

Use typed DTOs. SQLite row records are implementation details.

Recommended API DTOs:

```rust
pub struct SandboxSnapshot {
    pub sandbox_id: String,
    pub state: String,
    pub availability: SnapshotAvailability,
    pub sampled_at_unix_ms: Option<i64>,
    pub resources: Option<ResourceSnapshot>,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub partial_errors: Vec<SnapshotPartialError>,
}

pub enum SnapshotAvailability {
    Available,
    Partial,
    Unavailable,
}

pub struct WorkspaceSnapshot {
    pub workspace_id: String,
    pub state: String,
    pub remount_state: Option<String>,
    pub profile: Option<String>,
    pub sampled_at_unix_ms: Option<i64>,
    pub resources: Option<ResourceSnapshot>,
    pub active_namespace_executions: Vec<NamespaceExecutionSnapshot>,
    pub recent_traces: Vec<TraceSummary>,
    pub partial_errors: Vec<SnapshotPartialError>,
}

pub struct NamespaceExecutionSnapshot {
    pub namespace_execution_id: String,
    pub workspace_session_id: String,
    pub operation: String,
    pub lifecycle_state: String,
    pub sampled_at_unix_ms: Option<i64>,
    pub partial_errors: Vec<SnapshotPartialError>,
}

pub struct ResourceSnapshot {
    pub sampled_at_unix_ms: Option<i64>,
    pub cgroup_available: bool,
    pub cgroup_error: Option<String>,
    pub cpu_usage_usec: Option<i64>,
    pub memory_current_bytes: Option<i64>,
    pub memory_max_bytes: Option<i64>,
    pub memory_max_unlimited: Option<bool>,
    pub disk_upperdir_bytes: Option<i64>,
    pub disk_file_count: Option<i64>,
    pub disk_dir_count: Option<i64>,
    pub disk_symlink_count: Option<i64>,
    pub disk_truncated: Option<bool>,
    pub disk_read_error_count: Option<i64>,
    pub disk_first_error_path: Option<String>,
    pub history: Vec<ResourceSnapshotPoint>,
}

pub struct ResourceSnapshotPoint {
    pub sampled_at_unix_ms: i64,
    pub cpu_usage_usec: Option<i64>,
    pub memory_current_bytes: Option<i64>,
    pub disk_upperdir_bytes: Option<i64>,
}

pub struct TraceSummary {
    pub trace_id: String,
    pub kind: String,
    pub status: String,
    pub operation: String,
    pub request_id: Option<String>,
    pub origin_request_id: Option<String>,
    pub workspace_id: Option<String>,
    pub command_session_id: Option<String>,
    pub namespace_execution_id: Option<String>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub span_count: Option<u32>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

pub struct UnavailableSandboxSnapshot {
    pub sandbox_id: String,
    pub state: String,
    pub reason: SnapshotPartialError,
}

pub struct SnapshotPartialError {
    pub kind: String,
    pub message: String,
}
```

JSON field names should be stable API names, not raw SQLite column names where a
clearer name exists. Keep existing domain ids such as `sandbox_id`,
`workspace_id`, `command_session_id`, and `namespace_execution_id`.

The DTO hierarchy must preserve:

```text
sandbox_id
  state
  resources
  workspace_id
    state
    resources
    active namespace executions
    recent traces
```

Command work appears as a namespace execution with `operation == "exec_command"`.
The DTO does not expose a separate active command list, command text, or
command-session identity in active namespace execution rows.

Do not include:

- command output;
- stdin;
- environment variables;
- raw transcript rows;
- transcript contents;
- transcript paths in the first API;
- raw SQLite row objects;
- full file lists;
- full spans;
- unbounded error strings;
- endpoint auth tokens.

## Store Read APIs

Add the smallest production read surface in `crates/sandbox-observability`.
Prefer row-record reads or narrow summary records that the daemon maps into API
DTOs.

Required read helpers:

```text
load_sandbox_snapshot(sandbox_id)
load_workspace_snapshots(sandbox_id, workspace_id?)
load_namespace_execution_snapshots(sandbox_id, workspace_id?)
load_latest_resource_samples(sandbox_id, workspace_id?)
load_resource_samples(sandbox_id, workspace_id?, since_unix_ms, limit)
load_recent_trace_summaries(sandbox_id, workspace_id?, limit)
```

`load_recent_trace_summaries` should cover both ordinary `traces` rows and
`namespace_execution_traces` rows, or it should be implemented as two private
queries merged by the daemon projection. The public store API should remain one
bounded helper if that keeps daemon code simpler.

Reject these surfaces:

```text
query(sql, params)
connection()
raw_connection()
open_readonly_database_path(path)
manager_read_observability_database(...)
```

Existing indexes already support most reads:

- sandbox snapshot by primary key;
- workspace snapshots by `(sandbox_id, workspace_id)`;
- namespace execution snapshots by `(sandbox_id, workspace_session_id)`;
- latest and bounded resource samples by the resource sample time indexes;
- sandbox-level recent request/async traces by `idx_traces_sandbox_started`;
- namespace execution trace summaries by
  `idx_namespace_execution_traces_workspace_session_started`.

Add only one Phase 5 index unless profiling proves otherwise:

```sql
CREATE INDEX IF NOT EXISTS idx_traces_sandbox_workspace_started
  ON traces(sandbox_id, workspace_id, started_at_unix_ms);
```

Do not add command-session trace indexes until a command drilldown API exists.
Do not add span indexes beyond `idx_spans_trace_call_index`; Phase 5 returns
trace summaries, not span trees.

## Daemon Dispatch Shape

`get_observability_snapshot` is handled by the daemon before runtime dispatch.

Rust-like pseudocode:

```rust
impl SandboxDaemonServer {
    async fn dispatch_request(&self, request: Request) -> serde_json::Value {
        if let Err(response) = validate_daemon_scope(&request) {
            return response;
        }

        if request.op == "get_observability_snapshot" {
            let sandbox_id = match request.scope.sandbox_id() {
                Some(sandbox_id) => sandbox_id.to_owned(),
                None => unreachable!("validate_daemon_scope already rejected system scope"),
            };
            let config = self.config.clone();
            let observability = self.observability.clone();
            let operations = Arc::clone(&self.operations);
            let task = tokio::task::spawn_blocking(move || {
                dispatch_observability_snapshot(
                    sandbox_id,
                    observability,
                    &config,
                    &operations,
                    &request,
                )
            });
            return match task.await {
                Ok(response) => response.into_json_value(),
                Err(error) => daemon_internal_error(error),
            };
        }

        // Existing request trace creation and runtime dispatch path remains.
        let response = sandbox_runtime::dispatch_operation(
            &operations,
            &request,
            trace.as_ref(),
        );
        // Existing trace persistence and best-effort collection remain.
    }
}
```

The branch must:

- validate the request is sandbox scoped;
- parse and cap query args;
- return `Response::ok(json!({ "sandbox": snapshot }))` on available, partial,
  or unavailable snapshot nodes;
- use top-level faults only for invalid request syntax or internal task failure;
- never call `sandbox_runtime::dispatch_operation`;
- never create a runtime `OperationTrace`;
- never touch command transcript content.

Catalog/help decision:

- add a daemon-owned operation spec for `get_observability_snapshot`;
- expose it through a daemon catalog/helper surface, not through
  `sandbox_runtime::cli_operation_catalog()`;
- add protocol catalog/help support for daemon execution-space naming if needed
  by that helper;
- do not add a broad daemon operation registry unless a second daemon-owned
  operation appears.

## Manager Aggregation Shape

`get_observability_tree` is a manager-owned operation registered in the
management operation family.

Rust-like pseudocode:

```rust
fn dispatch_get_observability_tree(
    services: &ManagerServices,
    request: &Request,
) -> Response {
    let input = match parse_input(request) {
        Ok(input) => input.apply_caps(),
        Err(response) => return response,
    };

    let selected = match select_sandbox_records(&services.store, &input) {
        Ok(selected) => selected,
        Err(error) => return error.into_response(),
    };

    let snapshots = fan_out_bounded(
        selected,
        MAX_OBSERVABILITY_TREE_FANOUT,
        |selected| match selected {
            SelectedSandbox::Unavailable(node) => node.into_snapshot(),
            SelectedSandbox::Ready(record, endpoint) => {
                let daemon_request = Request::new(
                    "get_observability_snapshot",
                    child_request_id(&request.request_id, record.id.as_str()),
                    CliOperationScope::sandbox(record.id.as_str()),
                    snapshot_args(&input),
                );
                match services.daemon_client.invoke_with_timeout(
                    &endpoint,
                    daemon_request,
                    PER_DAEMON_OBSERVABILITY_TIMEOUT,
                ) {
                    Ok(response) => decode_snapshot_response(record.id.as_str(), response),
                    Err(error) => unavailable_from_daemon_error(record, error),
                }
            }
        },
    );

    Response::ok(json!({ "sandboxes": snapshots }))
}
```

This can run inside the existing manager `spawn_blocking` dispatch path. Do not
convert the whole manager operation dispatch stack to async for Phase 5.

Fan-out implementation rules:

- cap concurrency at `MAX_OBSERVABILITY_TREE_FANOUT`;
- preserve deterministic output order by collecting `(index, snapshot)` and
  sorting by `index` before response projection;
- represent per-daemon failures as unavailable nodes;
- bound daemon error strings before embedding them;
- never include endpoint auth tokens;
- never inspect daemon filesystem paths except as existing endpoint metadata.

## Detailed File Plan

`docs/observability/phase-5-manager-aggregation.md`

- This spec.

`crates/sandbox-protocol/src/catalog.rs`

- If daemon catalog exposure needs a distinct execution-space name, add
  `CliOperationExecutionSpace::Daemon`.
- Serialize it as `"daemon"`.
- Decode it from catalog JSON.
- Keep manager and runtime catalog behavior unchanged.

`crates/sandbox-protocol/src/help.rs`

- Render daemon catalog titles and help usage when a daemon catalog document is
  passed in.
- Do not add a CLI subcommand here; this only makes catalog rendering complete.

`crates/sandbox-protocol/tests/unit.rs`

- Update catalog tests so `"daemon"` is accepted when the enum is added.
- Keep an unknown value such as `"worker"` rejected.

`crates/sandbox-gateway/src/cli/request_builder.rs`

- Only needed if `CliOperationExecutionSpace::Daemon` is added.
- Make the request-builder match exhaustive by treating daemon catalog requests
  as sandbox-scoped requests, using the same sandbox id resolution as runtime
  operations.
- Do not add a new top-level `sandbox-cli daemon` command in Phase 5 unless a
  separate CLI product decision is made.

`crates/sandbox-daemon/src/server/dispatch.rs`

- Add the daemon-owned `get_observability_snapshot` branch after scope
  validation and before runtime trace creation/runtime dispatch.
- Keep the existing runtime dispatch path unchanged for all other operations.
- Project daemon query output through `Response::ok(result).into_json_value()`.

`crates/sandbox-daemon/src/observability/service.rs`

- Add `GetObservabilitySnapshotInput` parsing/projection helpers or a small
  nested query module if this file becomes too large.
- Add rate-limited query-triggered collection using existing collection
  semantics and cached disk samples.
- Add row-to-DTO projection.
- Add unavailable/partial snapshot builders for disabled observability and
  store read failures.
- Ensure command output and transcript content are never projected.

`crates/sandbox-observability/src/store.rs`

- Add the bounded production read helpers listed in the store-read section.
- Add `idx_traces_sandbox_workspace_started` only if the trace summary helper
  needs workspace-filtered request/async trace reads.
- Keep test-support read helpers test-gated.
- Do not expose a raw connection or SQL helper.

`crates/sandbox-observability/tests/schema.rs`

- Cover any new index in the allowed-index set.
- Add focused tests for production read helper ordering and bounds.
- Keep schema migration idempotency and checksum drift tests.

`crates/sandbox-manager/src/daemon_client.rs`

- Add `invoke_with_timeout` or the smallest timeout-aware equivalent needed by
  aggregation.
- Keep existing `invoke` for ordinary forwarding.

`crates/sandbox-manager/src/operation/impls/management/mod.rs`

- Add `mod get_observability_tree`.
- Add the operation spec to `SPECS`.
- Add the dispatch entry to `OPERATIONS`.
- Keep it in the existing `management` family.

`crates/sandbox-manager/src/operation/impls/management/get_observability_tree.rs`

- Parse request args and defaults.
- Validate and cap query options.
- Select ready records from `SandboxStore::list()`.
- Build daemon `get_observability_snapshot` requests.
- Run bounded fan-out through `SandboxDaemonClient`.
- Map daemon success to `SandboxSnapshot`.
- Map missing, stopped, failed, no-endpoint, timeout, unauthorized, malformed,
  and transport failures to unavailable nodes.
- Preserve output ordering.
- Avoid leaking auth tokens.

`crates/sandbox-manager/src/router/dispatch.rs`

- No routing semantic change should be needed once `get_observability_tree` is
  registered as manager-owned. Add tests only if registration exposes a router
  edge case.

`crates/sandbox-manager/tests/manager_core.rs`

- Add catalog coverage for `get_observability_tree`.
- Add direct operation tests for selection, duplicate id handling, stopped or
  missing sandbox nodes, deterministic order, all-daemon failure as partial
  success, and no auth token leakage.

`crates/sandbox-manager/tests/manager_router.rs`

- Add router coverage proving system-scoped `get_observability_tree` dispatches
  locally.
- Add coverage proving sandbox-scoped `get_observability_tree` is rejected as a
  manager operation.
- Add coverage proving sandbox-scoped `get_observability_snapshot` is forwarded
  rather than handled by the manager.

`crates/sandbox-daemon/tests/unit/observability.rs`

- Add daemon query tests for existing rows, disabled observability, capped
  limits, read failures, transcript/output exclusion, include flags, and unknown
  workspace behavior.

Files that should not change:

```text
crates/sandbox-runtime/operation/src/**
crates/sandbox-runtime/*/Cargo.toml
command transcript code
namespace-runner child process code
```

Phase 5 must not add production code or dependencies under
`crates/sandbox-runtime`.

## Expected LOC

Expected `crates/sandbox-runtime` change: 0 non-test LOC.

Expected non-runtime implementation shape:

```text
sandbox-observability read helpers and tests       120-220 non-test LOC
sandbox-daemon query branch and projection         160-280 non-test LOC
sandbox-manager aggregation operation             160-280 non-test LOC
protocol/catalog/help glue, if needed              20-60 non-test LOC
focused tests                                      as needed
```

If implementation needs any production change in `crates/sandbox-runtime`,
reject the architecture and simplify it before coding.

## Required Tests

Daemon tests:

- daemon snapshot query returns a bounded typed snapshot from existing
  observability rows;
- daemon snapshot query works when `include_recent_traces = false`;
- daemon caps excessive `trace_limit` and `resource_window_ms`;
- daemon returns an unavailable snapshot when observability is disabled;
- daemon returns a partial snapshot when SQLite reads fail but live runtime
  state can still be sampled;
- daemon query does not include transcript content, transcript rows, transcript
  paths, command output, stdin, stdout/stderr chunks, or environment variables;
- daemon query returns latest resources by default and bounded history only
  when `resource_window_ms` is provided;
- daemon query reuses cached disk samples and does not force a fresh disk walk
  on each query;
- daemon query returns a bounded `workspace_not_found` partial error for an
  unknown but valid `workspace_id`.

Manager tests:

- manager `get_observability_tree` selects ready sandboxes when
  `sandbox_ids = None`;
- manager `sandbox_ids = Some(..)` de-duplicates ids while preserving first
  occurrence order;
- manager maps stopped, failed, missing, no-endpoint, unreachable, unauthorized,
  timeout, malformed-response, and transport-error daemons to unavailable nodes;
- manager uses daemon client fan-out instead of opening SQLite;
- manager preserves deterministic result ordering;
- manager returns partial success with unavailable nodes when all selected
  daemons fail;
- manager does not leak auth tokens in responses or errors;
- router dispatches system-scoped `get_observability_tree` locally;
- router forwards sandbox-scoped `get_observability_snapshot`.

Store and dependency tests:

- production read helpers are bounded and deterministic;
- schema tests cover any new trace summary index;
- raw SQL query APIs and public raw connections are absent;
- no `sandbox-runtime` dependency on `sandbox-observability` is introduced.

## Verification Commands

Run after implementation:

```sh
cargo fmt --check
cargo check -p sandbox-observability --tests
cargo test -p sandbox-observability
cargo test -p sandbox-daemon observability
cargo test -p sandbox-manager manager_core
cargo test -p sandbox-manager manager_router
cargo clippy -p sandbox-manager --all-targets --no-deps -- -D warnings
cargo clippy -p sandbox-daemon --all-targets --no-deps -- -D warnings
cargo tree -p sandbox-runtime -i sandbox-observability
git diff --check
```

Additional package-scoped checks if catalog/help or gateway request-building
changes are made:

```sh
cargo test -p sandbox-protocol catalog
cargo test -p sandbox-gateway gateway_cli
```

The `cargo tree -p sandbox-runtime -i sandbox-observability` command should not
show `sandbox-runtime` depending on `sandbox-observability`. If the exact `cargo
tree` invocation exits nonzero because no inverse dependency exists, record that
as the expected proof rather than adding a runtime dependency.

## Completion Checklist

- [ ] daemon API `get_observability_snapshot` is specified as daemon-owned, not
  runtime-owned.
- [ ] manager API `get_observability_tree` is specified as manager-owned.
- [ ] manager aggregation queries daemon APIs and never opens or mirrors SQLite.
- [ ] authoritative daemon observability storage remains
  `/eos/runtime/daemon/observability/observability.sqlite`.
- [ ] `crates/sandbox-runtime` production LOC remains unchanged.
- [ ] response payloads remain direct `Response::ok(result)` values, with no
  `{ result, meta }` envelope.
- [ ] command transcripts and command output remain outside observability.
- [ ] query limits and daemon caps are specified.
- [ ] manager fan-out concurrency, timeout, partial failure, and deterministic
  ordering are specified.
- [ ] unavailable sandbox node shape is specified.
- [ ] raw SQL query APIs are rejected.
- [ ] Prometheus/Grafana/Loki/Tempo/OTLP integration remains out of scope.
- [ ] verification commands are listed and package-scoped.
