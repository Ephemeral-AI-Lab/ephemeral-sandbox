# Sandbox Observability Spec

Status: draft

Scope: local sandbox observability for `sandbox-manager`, `sandbox-daemon`,
and `sandbox-runtime`.

This spec defines a minimal observability design for many active sandboxes,
where each sandbox may have tens of active workspaces. The goal is to make
runtime state and method call chains visible without reintroducing the old
trace, event, or log pipeline.

## Goals

- Display active sandboxes in a stable hierarchy:

  ```text
  sandbox_id
    sandbox state
    sandbox global resource usage
    workspace_id
      workspace state
      resource usage
      active commands
      recent request and async method chains
  ```

- Collect a current snapshot for each sandbox and each active workspace.
- Track sandbox-global resource usage separately from per-workspace resource
  usage.
- Track CPU, memory, disk, cgroup, time, and runtime execution activity,
  including command activity, when the data is available.
- Record method call chains and elapsed time for each important method in an
  operation.
- Link async lifecycle work, such as command finalization, back to the
  originating command/workspace/request.
- Keep operation authors' tracing effort small when future operations are added.
- Store observability data locally and durably, with bounded retention.

## Non-Goals

- Do not change `sandbox_protocol::Response` payload shape.
- Do not add `{ result, meta }` envelopes for observability.
- Do not route command transcripts into observability storage.
- Do not require Prometheus, Grafana, Loki, Tempo, or OTLP for the first version.
- Do not create a global event bus.
- Do not store one method call per file.
- Do not make observability failures fail user operations.

## Current Boundary Facts

- `sandbox_id` is known at the manager/daemon boundary. Runtime requests carry
  `CliOperationScope::Sandbox { sandbox_id }`.
- `sandbox-runtime` currently owns workspace sessions, command state,
  layerstack operations, and runtime roots, but it does not own sandbox identity.
- `sandbox-manager` may create a per-sandbox daemon runtime directory when it
  launches many local daemon processes:

  ```text
  <manager_runtime_root>/<sandbox_id>/
    runtime.sock
    runtime.pid
  ```

  This is a manager launch convention for avoiding socket collisions, not an
  observability storage convention.
- `sandbox-daemon::ServerConfig` already carries `socket_path`, `pid_path`, and
  optional `sandbox_id`. The daemon runtime directory is
  `ServerConfig.socket_path.parent()`.
- Public runtime dispatch enters through `sandbox_runtime::dispatch_operation`.
- The current public runtime operation catalog is small. The live runtime
  operations are:

  ```text
  exec_command
  write_command_stdin
  read_command_lines
  squash
  ```

- Workspace session state is owned by `WorkspaceSessionService`.
- Current command execution state is owned by `CommandProcessStore`. Future
  namespace-runner operations should feed the same execution snapshot lane
  instead of creating one snapshot surface per operation class.
- Cgroup CPU/memory accounting is not currently present in `sandbox-runtime`.
  The daemon resource sampler should record cgroup data only when it can derive
  paths safely. When it cannot derive daemon-owned cgroup targets, unavailable
  cgroup samples are the expected behavior. `sandbox-runtime` should not grow a
  cgroup monitor for phase 1.

## `sandbox-runtime` Phase Boundary

The observability design must keep `crates/sandbox-runtime` as a runtime state
owner, not a database, metrics exporter, background monitoring crate, or
observability service host.

Phase 1 is data model and local SQLite store foundation only. It must add:

- `0` production LOC under `crates/sandbox-runtime`;
- `0` test LOC under `crates/sandbox-runtime`;
- no `sandbox-observability` dependency from `sandbox-runtime`;
- no SQLite imports, writer handles, observability paths, snapshot methods,
  trace context parameters, dispatch signature changes, or operation spans.

Later runtime adoption is split across later phases:

| Phase | Runtime scope | Expected runtime LOC |
| --- | --- | ---: |
| Phase 1 | Data model and local stores outside runtime | 0 |
| Phase 2 | Read-only workspace and execution snapshot adapters | 100-180 |
| Phase 3 | Request trace boundary and minimal selected operation spans | 70-120 |
| Phase 3.5 | Targeted in-process deep request spans | 60-130 |
| Phase 4 | Linked async finalization trace wiring | 60-110 |
| Phase 4.5 | Cross-process namespace-runner traces | 90-180 |

Anything else belongs outside `sandbox-runtime`:

- SQLite schema and store code live outside runtime.
- Sandbox identity, observability path derivation, and retention are
  daemon/observability concerns.
- Multi-sandbox aggregation belongs to `sandbox-manager`.
- Disk walking and cgroup file reads belong to daemon-side samplers.
- Prometheus/Grafana/Loki/OTLP integration stays optional and outside local
  runtime operation logic.

If a later implementation needs more runtime code than the table above, revise
the boundary before merging. The usual cause would be putting SQLite, samplers,
or a full trace storage model into `sandbox-runtime`.

## Components

### 1. Future Daemon Observability Service

Each sandbox daemon eventually owns a local `DaemonObservabilityService`.
This is not a Phase 1 implementation requirement. Phase 1 only creates the
local store foundation that the daemon can wire in later.

Responsibilities:

- Know the current `sandbox_id`.
- Derive the local observability directory.
- Own the local observability store.
- Collect daemon-local sandbox snapshots.
- Record method traces for requests handled by that daemon.
- Record linked async traces for later lifecycle work.
- Expose the daemon query API `get_observability_snapshot`.

The service runs inside the sandbox daemon because only the daemon has direct
access to its runtime operation state.

### 2. Manager Aggregator

The manager polls each ready sandbox daemon over its `runtime.sock` and builds
the global display tree.

Responsibilities:

- Discover ready sandboxes from the manager store.
- Call each sandbox daemon's observability snapshot API.
- Combine daemon snapshots into:

  ```text
  Vec<SandboxSnapshot>
  ```

- Expose the manager query API `get_observability_tree`.
- Never inspect workspace internals directly.
- Treat unavailable daemons as unavailable sandbox snapshots, not as empty
  sandboxes.

The manager may cache the aggregate in the future. Manager polling and
aggregation belong to Phase 5, not Phase 1.

### 3. Future Snapshot Collectors

Snapshot collectors are daemon-owned in Phase 2 and later. They read current
state through explicit runtime snapshot surfaces owned by existing service
owners:

- `WorkspaceStateSampler`: active workspace sessions and remount state.
- `ExecutionStateSampler`: active and recently completed runtime executions,
  with command executions as the current producer/display subset.
- `ResourceSampler`: CPU, memory, cgroup, disk, and time.
- `SandboxStateSampler`: daemon-level identity, runtime roots, and health.

Collectors are pull-based. They should not run a high-frequency background
monitor. Expensive resource sampling remains outside runtime. Phase 1 must not
add collectors, samplers, or runtime snapshot methods.

### 4. Future Method Trace Context

`OperationTrace` is a Phase 3 request-local span collector. The concrete storage,
writer, request metadata, response inspection, and hierarchy mapping live outside
`sandbox-runtime`; runtime only receives optional trace context and records
bounded method spans while it handles a request.

Responsibilities:

- Maintain request start time, a parent stack, completed spans, and stable
  `call_index` values.
- Create method spans with `enter` and `measure`.
- Finish spans on early return, panic unwind, or normal return.
- Hand completed runtime span DTOs to daemon-owned mapping code.
- Omit `trace_id`, `request_id`, `sandbox_id`, operation, workspace hierarchy,
  command hierarchy, response status, and response error metadata from runtime
  DTOs.

### 5. Local SQLite Store

Phase 1 creates one local SQLite foundation database:

```text
<daemon_runtime_dir>/observability/
  observability.sqlite
```

SQLite journal/WAL files may also appear beside it.

Writes from live daemon/runtime paths must be non-critical once those paths
exist:

- A failed observability write must not fail the user request.
- Trace writes should happen after response projection.
- State sample writes should be rate-limited once samplers exist.
- A bounded writer queue belongs to the first phase that introduces a hot live
  producer, not to Phase 1.

## Local Disk Layout

The daemon derives its observability directory from its daemon runtime
directory:

```text
daemon_runtime_dir = ServerConfig.socket_path.parent()
socket_path = <daemon_runtime_dir>/runtime.sock
observability_dir = <daemon_runtime_dir>/observability
```

Production config can set the daemon runtime directory under `/eos`:

```text
/eos/runtime/daemon/
  runtime.sock
  runtime.pid
  observability/
    observability.sqlite
```

When the manager launches many local sandboxes, it can choose a host-side
daemon runtime directory such as `<manager_runtime_root>/<sandbox_id>/`. In that
case `<sandbox_id>` belongs to the manager-selected daemon runtime directory,
not to `sandbox-observability` path derivation.

Do not store observability data inside:

- a workspace upperdir;
- command scratch directories;
- `transcript.log`;
- layerstack storage.

Those paths have functional lifecycle semantics and may be destroyed, captured,
or published independently of observability.

## Target Table Family: Method Trace

Phase 1 creates the minimal trace tables inside `observability.sqlite`. The
larger schema below is a later target shape after request, async, hierarchy query
paths, and linked runner trace producers exist. Phase 3 request traces use the
existing Phase 1 `traces` and `spans` shape; they do not add hierarchy columns or
indexes.

Purpose:

- Request method chains.
- Async method chains.
- Per-method duration.
- Request id to method chain lookup.
- Command finalization and other linked async lifecycle work.

Recommended pragmas:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 1000;
PRAGMA foreign_keys = ON;
```

Schema:

```sql
CREATE TABLE IF NOT EXISTS traces (
  trace_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,              -- request | async
  status TEXT NOT NULL,            -- running | ok | error | dropped
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT,
  command_session_id TEXT,
  correlation_kind TEXT,
  correlation_id TEXT,
  operation TEXT NOT NULL,
  request_id TEXT,
  origin_request_id TEXT,
  async_name TEXT,
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
  status TEXT NOT NULL,            -- running | ok | error | dropped
  started_at_unix_ms INTEGER NOT NULL,
  finished_at_unix_ms INTEGER,
  duration_ms REAL,
  error_kind TEXT,
  error_message TEXT,
  FOREIGN KEY(trace_id) REFERENCES traces(trace_id) ON DELETE CASCADE,
  FOREIGN KEY(parent_span_id) REFERENCES spans(span_id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS trace_links (
  trace_id TEXT NOT NULL,
  link_kind TEXT NOT NULL,         -- origin_request | command | workspace
  target_id TEXT NOT NULL,
  PRIMARY KEY(trace_id, link_kind, target_id),
  FOREIGN KEY(trace_id) REFERENCES traces(trace_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_traces_request
  ON traces(request_id);

CREATE INDEX IF NOT EXISTS idx_traces_origin_request
  ON traces(origin_request_id);

CREATE INDEX IF NOT EXISTS idx_traces_workspace_time
  ON traces(sandbox_id, workspace_id, started_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_traces_command_time
  ON traces(sandbox_id, command_session_id, started_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_traces_correlation_time
  ON traces(sandbox_id, correlation_kind, correlation_id, started_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_spans_trace_call_index
  ON spans(trace_id, call_index);
```

Retention:

- Keep active traces until they finish or time out.
- Keep a configurable count or time window for completed traces.
- Suggested default: last 10,000 request traces and last 10,000 async traces
  per sandbox.
- Delete old traces by deleting from `traces`; `spans` and `trace_links` should
  cascade.

## Target Table Family: Sandbox State

Phase 1 creates only a minimal `sandbox_snapshots` table inside
`observability.sqlite`. The larger schema below is the Phase 2+ target shape,
after runtime snapshot adapters and resource samplers exist.

Purpose:

- Current sandbox snapshot.
- Current workspace snapshots.
- Recent resource samples.
- Current active runtime execution snapshots.

`sandbox_snapshots` is only the root row for a sandbox. It is enough for Phase 1
store bootstrapping, but it is not enough for the full observability hierarchy.
The complete local model needs these companion table families:

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
  traces + spans          -> live coarse request/method-chain tracing from operations

Phase 3.5
  traces + spans          -> targeted in-process child spans under slow request spans

Phase 4
  trace_links             -> async relationships back to requests/commands

Phase 4.5
  traces + spans          -> linked namespace-runner process traces
  trace_links             -> runner traces back to request/command ownership
```

Phase 1 therefore establishes storage shape for `sandbox_snapshots`, `traces`,
and `spans`, but it does not make any of them live observability producers.

Recommended pragmas:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 1000;
PRAGMA foreign_keys = ON;
```

Schema:

```sql
CREATE TABLE IF NOT EXISTS sandbox_snapshots (
  sandbox_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,             -- ready | unavailable | stopping | failed
  workspace_root TEXT,
  daemon_runtime_dir TEXT,
  socket_path TEXT,
  pid_path TEXT,
  daemon_pid INTEGER,
  sampled_at_unix_ms INTEGER NOT NULL,
  error_message TEXT
);

CREATE TABLE IF NOT EXISTS workspace_snapshots (
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  state TEXT NOT NULL,             -- active | remount_pending | remount_blocked | destroying
  profile TEXT,
  workspace_root TEXT,
  upperdir TEXT,
  workdir TEXT,
  holder_pid INTEGER,
  namespace_fd_count INTEGER,
  readiness_fd_open INTEGER,
  control_fd_open INTEGER,
  base_manifest_version INTEGER,
  base_root_hash TEXT,
  layer_count INTEGER,
  created_at_unix_ms INTEGER,
  last_activity_unix_ms INTEGER,
  lifetime_ms REAL,
  sampled_at_unix_ms INTEGER NOT NULL,
  PRIMARY KEY(sandbox_id, workspace_id)
);

CREATE TABLE IF NOT EXISTS resource_samples (
  sample_id TEXT PRIMARY KEY,
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT,               -- NULL for sandbox-global sample
  sampled_at_unix_ms INTEGER NOT NULL,

  cgroup_path TEXT,
  cgroup_available INTEGER NOT NULL,
  cgroup_error TEXT,

  cpu_usage_usec INTEGER,
  cpu_user_usec INTEGER,
  cpu_system_usec INTEGER,
  cpu_throttled_usec INTEGER,
  cpu_nr_throttled INTEGER,

  memory_current_bytes INTEGER,
  memory_peak_bytes INTEGER,
  memory_max_bytes INTEGER,
  memory_oom_events INTEGER,

  disk_upperdir_bytes INTEGER,
  disk_file_count INTEGER,
  disk_dir_count INTEGER,
  disk_symlink_count INTEGER,
  disk_truncated INTEGER,
  disk_read_error_count INTEGER,
  disk_first_error_path TEXT
);

CREATE TABLE IF NOT EXISTS execution_snapshots (
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  execution_kind TEXT NOT NULL,    -- command | future operation-neutral kinds
  operation TEXT,
  command_session_id TEXT,
  namespace_runner_request_id TEXT,
  command TEXT,
  lifecycle_state TEXT NOT NULL,   -- running | quiesced_for_remount | finalizing | cancelled
  finalization_state TEXT NOT NULL,
  workspace_ownership TEXT,
  started_at_unix_ms INTEGER,
  wall_time_ms REAL,
  command_total_time_ms REAL,
  process_group_id INTEGER,
  transcript_path TEXT,
  sampled_at_unix_ms INTEGER NOT NULL,
  PRIMARY KEY(sandbox_id, execution_id)
);

CREATE INDEX IF NOT EXISTS idx_workspace_snapshots_sandbox
  ON workspace_snapshots(sandbox_id, workspace_id);

CREATE INDEX IF NOT EXISTS idx_resource_samples_workspace_time
  ON resource_samples(sandbox_id, workspace_id, sampled_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_resource_samples_sandbox_time
  ON resource_samples(sandbox_id, sampled_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_execution_snapshots_workspace
  ON execution_snapshots(sandbox_id, workspace_id);

CREATE INDEX IF NOT EXISTS idx_execution_snapshots_command
  ON execution_snapshots(sandbox_id, command_session_id);
```

Retention:

- `sandbox_snapshots`, `workspace_snapshots`, and `execution_snapshots` represent
  current state and are updated in place.
- `resource_samples` is time-series data and should be retained by time window.
- A `resource_samples` row with `workspace_id IS NULL` is the sandbox-global
  sample for that sandbox.
- A `resource_samples` row with `workspace_id IS NOT NULL` is a per-workspace
  sample under that sandbox.
- Suggested default: retain the last 30 minutes of resource samples per sandbox.
- Disk samples may be sampled less often than cgroup samples.

## Snapshot DTOs

The daemon API should expose typed DTOs. SQLite is an implementation detail.

```rust
pub struct SandboxSnapshot {
    pub sandbox_id: String,
    pub state: SandboxStateView,
    pub resources: ResourceSnapshot,
    pub sampled_at_unix_ms: i64,
    pub daemon_runtime_dir: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub workspaces: Vec<WorkspaceSnapshot>,
}

pub struct WorkspaceSnapshot {
    pub workspace_id: String,
    pub state: WorkspaceStateView,
    pub profile: Option<String>,
    pub base_revision: Option<BaseRevisionView>,
    pub resources: ResourceSnapshot,
    pub active_executions: Vec<ExecutionSnapshot>,
    pub recent_traces: Vec<TraceSummary>,
}

pub struct ExecutionSnapshot {
    pub execution_id: String,
    pub execution_kind: String,
    pub operation: Option<String>,
    pub workspace_id: String,
    pub command_session_id: Option<String>,
    pub namespace_runner_request_id: Option<String>,
    pub command: Option<String>,
    pub lifecycle_state: String,
    pub finalization_state: String,
    pub workspace_ownership: Option<String>,
    pub wall_time_ms: Option<f64>,
    pub process_group_id: Option<i32>,
    pub transcript_path: Option<PathBuf>,
}

pub struct ResourceSnapshot {
    pub cgroup: CgroupSnapshot,
    pub cpu: CpuSnapshot,
    pub memory: MemorySnapshot,
    pub disk: DiskSnapshot,
    pub time: TimeSnapshot,
}
```

The manager renders these DTOs in the required hierarchy:

```text
sandbox_id
  state
  resources
  workspace_id
    state
    resources
    active executions
    active commands (filtered from active executions)
    recent traces
```

## Query API

SQLite is the daemon-local implementation detail. Product code should query
observability through daemon and manager operations, not by opening SQLite
directly from the host.

Direct SQLite reads are allowed for local debugging, schema migration tests, and
emergency inspection only. The stable query contract is typed DTOs over the
existing daemon socket path.

### Daemon Query: `get_observability_snapshot`

Execution space:

```text
sandbox daemon over <daemon_runtime_dir>/runtime.sock
```

Purpose:

- Query one sandbox daemon.
- Return one `SandboxSnapshot`.
- Optionally narrow to one workspace.
- Optionally include recent traces and bounded resource history.

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

Rules:

- `workspace_id = None` returns sandbox-global state plus all active workspace
  summaries.
- `workspace_id = Some(..)` returns sandbox-global state plus only that
  workspace when it exists.
- `include_resources = true` includes sandbox-global resources and workspace
  resources.
- `include_recent_traces = true` includes recent request and async trace
  summaries.
- `resource_window_ms` bounds resource time-series rows. If omitted, return
  current/latest resources only.
- `trace_limit` is capped by the daemon even if the caller asks for more.
- Missing cgroup paths return unavailable resource snapshots; they do not fail
  the query.
- SQLite lock or read failures return a partial snapshot with bounded error
  fields when current runtime state can still be collected.

### Manager Query: `get_observability_tree`

Execution space:

```text
manager on the host
```

Purpose:

- Query all ready sandbox daemons.
- Build the global display tree.
- Represent daemon failures as unavailable sandbox nodes.

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

Rules:

- `sandbox_ids = None` queries every ready sandbox known to the manager.
- `sandbox_ids = Some(..)` queries only those sandboxes.
- The manager fans out by calling `get_observability_snapshot` on each daemon.
- The manager should not open per-sandbox SQLite files.
- The manager may cache the aggregate later, but the first implementation can
  query daemons on demand.

### Later Drill-Down Queries

Do not add these until the snapshot API proves too coarse:

```text
get_method_trace(trace_id)
list_method_traces(request_id?, workspace_id?, command_session_id?, limit?)
get_resource_samples(workspace_id?, window_ms)
```

These later queries should still go through the daemon or manager API. They
should not expose raw SQL to callers.

## Resource Sampling

### Cgroups

Cgroup support is optional in the first implementation.

Sampling has two levels:

- Sandbox-global cgroup sample:
  - `sandbox_id` is set.
  - `workspace_id` is `NULL`.
  - The cgroup path points at the sandbox/daemon/container-level cgroup.
- Workspace cgroup sample:
  - `sandbox_id` is set.
  - `workspace_id` is set.
  - The cgroup path points at that workspace's cgroup, when such a path exists.

The daemon-side cgroup sampler should model the target explicitly:

```rust
pub type SandboxId = String;
pub type WorkspaceId = String;

pub enum CgroupSampleTarget {
    Sandbox {
        sandbox_id: SandboxId,
        cgroup_path: PathBuf,
    },
    Workspace {
        sandbox_id: SandboxId,
        workspace_id: WorkspaceId,
        cgroup_path: PathBuf,
    },
}
```

`CgroupSampleTarget::Sandbox` writes a `resource_samples` row with
`workspace_id = NULL`. `CgroupSampleTarget::Workspace` writes a
`resource_samples` row with `workspace_id` set.

When cgroup paths are known:

- Record `cgroup_path`.
- Read cgroup v2 files if present:
  - `cpu.stat`
  - `memory.current`
  - `memory.peak`
  - `memory.max`
  - `memory.events`
  - `io.stat` if disk IO is needed later.
- On read error, keep the resource sample with `cgroup_available = 0` and a
  bounded `cgroup_error`.

When cgroup paths are not known:

- Set `cgroup_available = 0`.
- Set `cgroup_error = "cgroup path unavailable"`.
- Do not fail the snapshot.

This is the current Phase 2 implementation state: daemon resource sampling writes
unavailable cgroup samples because no explicit daemon-owned cgroup targets are
derived yet.

Do not synthesize per-workspace cgroup usage from a sandbox-global cgroup. If
workspace processes are not placed in distinct cgroups, record workspace cgroup
samples as unavailable and keep only the sandbox-global cgroup sample.

Do not compute sandbox-global usage by summing workspace cgroup rows unless the
cgroup hierarchy guarantees that the workspace cgroups are exactly the full
child set of the sandbox cgroup.

### CPU

Preferred source: cgroup v2 `cpu.stat`.

Fields:

- `usage_usec`
- `user_usec`
- `system_usec`
- `nr_throttled`
- `throttled_usec`

CPU percentage can be computed by the UI or manager from two samples. The
daemon should store raw counters.

### Memory

Preferred source: cgroup v2 memory files.

Fields:

- `memory.current`
- `memory.peak`
- `memory.max`
- `oom` count from `memory.events`

### Disk

Preferred source: workspace upperdir tree stats.

Fields:

- bytes
- files
- directories
- symlinks
- truncated
- read error count
- first error path

Disk sampling can be expensive. The daemon should rate-limit disk sampling and
reuse recent disk samples in the current snapshot.

### Time

Use monotonic time for durations and Unix milliseconds for storage/display.

Fields:

- workspace created time
- last activity
- workspace lifetime
- command wall time
- command total time
- method duration

## Method Trace Model

The unit of method trace storage is a method chain, not an individual method
file.

A trace contains:

- one `trace_id`;
- one `request_id` for synchronous request traces;
- optional `origin_request_id` for async traces;
- `sandbox_id`;
- optional `workspace_id`;
- optional `command_session_id`;
- operation name;
- ordered spans.

Phase 3 request traces populate the existing request fields only. Optional
`workspace_id`, `command_session_id`, correlation, and async fields are later
query/correlation concerns and must not be added by Phase 3.

A span contains:

- `span_id`;
- `parent_span_id`;
- `method_name`;
- start time;
- finish time;
- duration;
- status;
- optional error.

## Mapping Method Spans to Request IDs

Create the trace context at the daemon operation boundary only when daemon
observability is enabled and `sandbox_id` is available.

```rust
let trace = observability_enabled.then(OperationTrace::new);
```

Then pass `trace.as_ref()` through operation dispatch:

```rust
entry.dispatch(operations, request, trace.as_ref())
```

Every call to `trace.enter(...)` or `trace.measure(...)` writes timing, nesting,
and `call_index` into the same runtime span collector. Runtime does not store:

```text
trace_id
request_id
sandbox_id
operation
workspace_id
command_session_id
response status
response error metadata
```

After response projection, daemon-owned mapping derives:

```text
trace_id = "request:" + request_id
span_id = trace_id + ":span:" + call_index
parent_span_id = trace_id + ":span:" + parent_call_index
kind = "request"
status/error = projected response JSON
```

Phase 3 intentionally does not add trace hierarchy fields or indexes for
`workspace_id` / `command_session_id`. Those fields are reconsidered with the
first daemon trace query API or an explicit hierarchy-correlation phase.

## `enter` vs `measure`

Use `enter` for scopes and parent methods.

Use `measure` for one call or expression.

Rules:

- Public operation dispatch methods: `enter`.
- Public service calls made by operation dispatch wrappers: `measure`.
- Important subcalls inside a service method: Phase 3.5 only after evidence.
- Tiny helpers: no span.
- Loops: one span around the loop, not one span per item.
- Async task entrypoints: Phase 4 linked traces, not Phase 3 child spans.
- Cleanup/finalization paths: Phase 4 linked traces unless they are synchronous
  request work.

Example:

```rust
let result = trace
    .map(|trace| {
        trace.measure("CommandOperationService::exec_command", || {
            operations.command.exec_command(input)
        })
    })
    .unwrap_or_else(|| operations.command.exec_command(input));
```

## Future Operation Authoring Rule

A new operation should get root timing for free.

The dispatcher creates:

```text
dispatch_operation
  <operation>::dispatch
```

The operation author should add at most one public service-method span at the
operation dispatch wrapper. Do not add trace parameters to public service
methods in Phase 3.

Minimum expected effort for a new operation:

```rust
trace
    .map(|trace| {
        trace.measure("NewOperationService::new_operation", || {
            operations.new_operation.execute(input)
        })
    })
    .unwrap_or_else(|| operations.new_operation.execute(input))
```

If the route author adds no spans, operation-level timing still exists. If they
add one public service-method span, the UI gets a readable method chain. Deeper
subcall spans belong to Phase 3.5.

## `exec_command` Method Chain

Synchronous request chain:

```text
exec_command request
  dispatch_operation
    command::exec_command::dispatch
      parse_input
      CommandOperationService::exec_command
        validate_command
        exec_validated_command
          resolve_exec_workspace
            if workspace_session_id:
              WorkspaceSessionService::resolve_session
            else:
              WorkspaceSessionService::create_workspace_session
                WorkspaceRuntimeService::create_workspace
          command_admission_guard
            ensure_workspace_session_not_remount_pending
          allocate_command_session_id
          process_store.try_reserve
          start_command_process
            WorkspaceHandle::entry
            launch_driver.spawn
              CommandProcessSpawn::prepare
              CommandProcess::spawn
                build_namespace_runner_request
                spawn_current_exe_ns_runner
          process_store.insert_active
          start_completion_watcher
          initial_exec_yield
            wait_for_command_yield
              wait_for_completion_yield
              running_or_completed_command_yield
      command_yield_response
        Response::running or Response::ok
```

Recommended Phase 3 coarse spans:

```text
dispatch_operation
exec_command::dispatch
CommandOperationService::exec_command
```

Phase 3 spans are inclusive timings. For example,
`CommandOperationService::exec_command` includes validation, workspace
resolution, command admission, command process start, watcher launch, and initial
yield. Phase 3 must not record the full internal workspace, layerstack, command
spawn, namespace runner, or shell execution call chain.

Do not instrument these helper-sized boundaries on the first pass:

```text
parse_input
resolve_exec_workspace
start_command_process
initial_exec_yield
write_or_cancel
wait_for_command_yield
read_transcript_window
command_admission
register_active_command
start_completion_watcher
command_yield_response
```

## Targeted Deep Request Spans

Phase 3.5 may add lower-level in-process spans only when Phase 3 coarse traces
show that a parent span is frequently slow or ambiguous. These spans stay on the
same request trace and must still be method-chain observability, not
profiler-style tracing.

Phase 3.5 must use a generic, automatic, and dynamic deep-span mechanism rather
than hard-coding a fixed always-on child-span list. The runtime should expose one
small trace API for optional child spans, such as `measure_if`, backed by a
request-local enabled-key set. Operation code names exported stable span-key
constants at meaningful boundaries, and the enabled set decides at request time
whether each span key is recorded.

Keep the first Phase 3.5 implementation deliberately simple:

- use one API for all child spans;
- use one stable span-key namespace;
- let `OperationTrace` carry one immutable enabled-key set for the request;
- keep using the existing `traces` and `spans` storage path;
- build the first enabled-key set from recent local trace statistics;
- when a span key is disabled, run the original code path without recording a
  child span.

The dynamic behavior should be a small enablement decision, not a new adaptive
observability subsystem. For example, the daemon may start with Phase 3 coarse
spans only, then enable a registered child span group when recent local traces
show the parent span is slow or ambiguous. Operation authors should not hand-edit
instrumentation just because one sandbox is currently slow.

An acceptable first implementation can be a daemon-local in-memory enabled-key
set updated after completed request traces. It does not need config, new tables,
a background worker, percentile math, a new query API, or a separate tuning
service.

Do not attempt generic automatic discovery of every Rust function call in Phase
3.5. That would require broad `tracing` attribute adoption, compiler
instrumentation, eBPF/profiler integration, or invasive function signature churn,
and it would produce profiler-style data rather than readable method chains.
Phase 3.5 genericity is in the enabled-key set and API, while eligible span
points remain explicit and stable.

Phase 3.5 candidates:

```text
command exec workspace resolution
  existing session resolution
  one-shot session creation

command exec process start

layerstack squash
  open stack
  compact stack
```

Phase 3.5 rules:

- Add a child span only under a Phase 3 parent span that has proven useful to
  split.
- Add child spans through the generic enabled-key-gated API, not through fixed
  always-on instrumentation.
- Keep each request trace readable; do not expand every helper or loop.
- Use stable domain span keys such as `command.exec.workspace.resolve` or
  `layerstack.squash.compact_stack`; display names can be derived from those
  keys or set beside them.
- Keep enabled-key state outside lower runtime crates. Lower crates may receive
  only neutral trace context when their existing API boundary can accept it
  cleanly.
- Do not pass SQLite stores, daemon paths, writer handles, or
  `sandbox-observability` types into lower runtime crates.
- Do not create a dependency from lower workspace, layerstack, command, or
  namespace crates back to the operation crate just to carry trace context.
- If a lower crate boundary cannot accept trace context cleanly, keep the span
  at the caller-owned boundary until a dedicated design revises that API.

## Async Chains

Async lifecycle work must be represented as linked traces, not as children of a
request trace that already returned a response.

Example:

```text
request trace
  request_id=req_1
  operation=exec_command
  command_session_id=cmd_1
  response=running

linked async trace
  kind=async
  async_name=command_finalization
  origin_request_id=req_1
  correlation_kind=command_session_id
  correlation_id=cmd_1
  command_session_id=cmd_1
  workspace_id=ws_1
```

Async command finalization chain:

```text
command_finalization
  completion_watcher
    CommandProcess::take_exit
    CommandCompletionPromise::resolve
  completion_finalizer
    complete_terminal_command_with_services
      begin_terminal_completion
      terminal_result
      apply_workspace_completion_policy
        if one-shot:
          WorkspaceSessionService::destroy_session
            WorkspaceRuntimeService::destroy_workspace
      complete_command_record
```

The link keys are:

```text
sandbox_id
workspace_id
command_session_id
correlation_kind
correlation_id
origin_request_id
```

The UI should render the async chain under the same workspace and command as
the original request when those ids are present. For async work that is not
owned by a command, the UI should use the correlation key and any available
sandbox/workspace ids.

## Request and Async Trace IDs

Request trace id:

```text
trace_id = "request:" + request_id
```

Async trace id:

```text
trace_id = "async:" + async_name + ":" + correlation_kind + ":" + correlation_id
```

The correlation key is the narrowest stable id that owns the async work.
`command_session_id` is appropriate for command finalization, but other async
work should use the id that best describes ownership:

```text
async:command_finalization:command_session_id:cmd_1
async:workspace_destroy:workspace_id:ws_1
async:workspace_remount:workspace_id:ws_1
async:lease_cleanup:lease_id:lease_42
async:sandbox_shutdown:sandbox_id:sbox_1
```

If there can be multiple async traces for the same async name and correlation
key, add a monotonic sequence:

```text
async:command_finalization:command_session_id:cmd_1:1
```

Span ids should be trace-local monotonic ids or globally unique strings:

```text
span_id = trace_id + ":" + call_index
```

## Cross-Process Namespace Runner Traces

Phase 4.5 may record namespace-runner internals, but those records must be
linked cross-process traces, not ordinary child spans that imply the daemon can
directly observe a child process call stack.

Phase 3 and Phase 3.5 parent-side request spans may record:

```text
start_command_process
  build_namespace_runner_request
  spawn_current_exe_ns_runner
```

Those spans stop at the parent/runtime process boundary. They must not expand
into `runner::run`, `run_setns`, shell execution, or wait-loop internals.

Phase 4.5 runner trace candidates:

```text
namespace_runner
  runner::run
  run_setns
  shell_exec::execute_shell
  wait_for_command_execution_scope
```

Phase 4.5 requirements:

- Propagate bounded trace metadata through the namespace-runner request before
  recording runner spans.
- Link runner traces back to the original request and command using
  `origin_request_id`, `command_session_id`, and a stable
  `namespace_runner_request_id` or equivalent runner request id.
- Treat a runner trace as an independent linked trace when the runner can outlive
  the request that spawned it.
- Keep command stdout/stderr and transcript content out of the trace database.
- Do not make the namespace-runner child process write directly to
  `observability.sqlite`.
- Define the collection transport in the Phase 4.5 spec before implementation;
  acceptable designs must be bounded and must not require OTLP, Loki, Tempo, or
  command transcript ingestion.
- Record method names, status, timing, and bounded errors only; do not record
  shell command output chunks, transcript lines, environment dumps, or every
  wait-loop iteration.

## Active vs Completed Traces

This is Phase 3+ behavior, not Phase 1 behavior.

Active traces may be kept in memory and periodically flushed to SQLite with
`status = running`.

On completion:

- update the root trace row;
- insert or update all spans;
- set `finished_at_unix_ms`;
- set `duration_ms`;
- set `status`.

If the daemon crashes during a request, the next startup may leave older
`running` traces as `dropped` after a timeout.

## Current Snapshot Collection

This is Phase 2+ behavior, not Phase 1 behavior.

The daemon should collect snapshots on demand and optionally on a timer.

Recommended behavior:

- Fast state fields: collect every request for snapshot API.
- Cgroup CPU/memory: collect at most once per second per workspace.
- Disk upperdir stats: collect at most once every 10 to 30 seconds per
  workspace.
- Method traces: record on request completion, async completion, and later
  linked runner completion.

Snapshot calls should return cached samples when a sampler is rate-limited.

## Failure Policy

Observability must be best effort.

- If a sampler fails, include the error in that sampler's fields.
- If SQLite is locked, retry briefly and then drop the record.
- If a future writer queue is full, drop the oldest unflushed observability
  item.
- If the local store fails, disable live observability writes until a later
  initialization or health check succeeds.
- Never fail `exec_command`, `write_command_stdin`, `read_command_lines`,
  `squash`, or workspace lifecycle work because observability failed.

## Prometheus, Grafana, and Loki

First version:

- No Prometheus requirement.
- No Grafana requirement.
- No Loki requirement.
- No OTLP requirement.

Future optional integrations:

- Prometheus can scrape numeric gauges/counters derived from local state tables.
- Grafana can visualize Prometheus metrics and possibly query SQLite through an
  adapter if needed.
- Loki should remain out of this design unless the project explicitly wants
  searchable logs again.

Prometheus is not a good storage format for method call chains. Keep method
chains in the local SQLite store.

## Security and Data Volume

Do not store:

- full command output;
- full stdin;
- full environment variables;
- unbounded error strings;
- full file lists from disk scans.

Bound these fields:

- method name length;
- error message length;
- path string length;
- number of recent traces returned by snapshot API;
- number of resource samples retained.

Command output remains in command transcript artifacts, not in observability
databases.

## Rollout Plan

### Phase 1: Data Model and Stores

- Add a minimal `sandbox-observability` crate.
- Add row-shaped records for traces, spans, and sandbox snapshots.
- Derive the observability directory from the daemon socket path.
- Create `observability.sqlite`.
- Add idempotent schema migration and direct synthetic-record store writes.
- Do not wire the crate into daemon serving yet.
- Expected `crates/sandbox-runtime` change: 0 LOC.

### Phase 2: Runtime Snapshots

- Add one read-only runtime snapshot method.
- Snapshot active workspaces from `WorkspaceSessionService` through that method.
- Snapshot active and recent runtime executions through a shared execution
  snapshot lane. `CommandProcessStore` is the current producer because
  `exec_command` is the current long-running namespace-runner operation.
- Add daemon-side disk stats using existing upperdir paths.
- Add daemon-side cgroup sample shape with unavailable state while explicit
  daemon-owned cgroup targets are absent.
- Expected `crates/sandbox-runtime` change: 100-180 non-test LOC.

### Phase 3: Coarse Request Method Traces

- Create `OperationTrace` at daemon dispatch only when daemon observability is
  enabled and `sandbox_id` is present.
- Pass optional trace context into runtime dispatch.
- Add automatic root dispatch span.
- Add one public service-method span for `exec_command`, `write_command_stdin`,
  `read_command_lines`, and `squash`.
- Persist completed request traces and spans in `sandbox-daemon`.
- Do not add Phase 3 trace hierarchy columns or indexes.
- Do not expand into full workspace, layerstack, command spawn,
  namespace-runner, or shell execution internals.
- Expected `crates/sandbox-runtime` change: 70-120 non-test LOC, with 75-95
  preferred.

### Phase 3.5: Targeted Deep Request Spans

- Add a generic enabled-key-gated child span API, such as `measure_if`, and a
  stable span-key constant namespace for eligible in-process boundaries.
- Derive deep-span enablement automatically and dynamically from daemon-local
  recent trace statistics.
- Keep the first implementation to an enabled-key set; do not add config, new
  schema, background workers, percentile math, or query APIs for Phase 3.5.
- Split proven-slow Phase 3 parent spans into a small number of lower-level
  in-process child spans by enabling registered span keys, not by hard-coding an
  always-on list.
- Candidate areas are workspace session creation/resolution, a broad parent-side
  command process-start boundary, and layerstack squash internals. Defer
  workspace runtime creation, layerstack snapshot or lease acquisition,
  launch-driver internals, and namespace-runner request building until the broad
  first-pass spans prove too ambiguous.
- Keep deep spans on the same request trace when the work runs in the daemon or
  runtime process.
- Do not push SQLite, daemon observability stores, writer handles, or
  `sandbox-observability` types into lower runtime crates.
- Do not add cross-process namespace-runner child execution spans in this phase.
- Expected `crates/sandbox-runtime` change: 60-130 non-test LOC.

### Phase 4: Async Method Traces

- Add linked traces for command finalization.
- Link by `origin_request_id` plus `correlation_kind` / `correlation_id`, with
  `workspace_id` and `command_session_id` populated when known.
- Add linked traces for workspace destroy/remount if those run outside the
  original request in future code.
- Expected `crates/sandbox-runtime` change: 60-110 non-test LOC.

### Phase 4.5: Cross-Process Namespace Runner Traces

- Propagate bounded trace metadata into namespace-runner requests.
- Record linked runner traces for child-process internals such as `runner::run`,
  `run_setns`, `shell_exec::execute_shell`, and
  `wait_for_command_execution_scope`.
- Link runner traces back to the original request and command by
  `origin_request_id`, `command_session_id`, and a stable runner request id.
- Define a bounded collection transport before implementation; the runner child
  must not write directly to `observability.sqlite`.
- Keep command transcripts and command output out of observability storage.
- Expected `crates/sandbox-runtime` change: 90-180 non-test LOC.

### Phase 5: Manager Aggregation

- Add daemon API `get_observability_snapshot`.
- Add manager API `get_observability_tree`.
- Add manager aggregation across ready sandboxes.
- Render hierarchy:

  ```text
  sandbox_id
    state
    resources
    workspace_id
      state
      resources
      active executions
      active commands (filtered from active executions)
```
- Expected `crates/sandbox-runtime` change: 0 LOC.

### Phase 6: Optional Metrics Export

- Add Prometheus export only after the local snapshot and trace model are
  stable.
- Export aggregate numeric metrics only.
- Keep method traces in SQLite.
- Expected `crates/sandbox-runtime` change: 0 LOC.

## Verification

Focused checks after implementation should include:

```sh
cargo fmt --check
cargo check -p sandbox-observability --tests
cargo test -p sandbox-observability
```

Storage-specific tests should verify:

- SQLite schema migration creates `observability.sqlite`.
- Running schema initialization twice is a no-op.
- A synthetic request trace maps all spans to the same `trace_id`.
- A synthetic sandbox snapshot upsert replaces the current row.
- `crates/sandbox-runtime` remains untouched by Phase 1.
