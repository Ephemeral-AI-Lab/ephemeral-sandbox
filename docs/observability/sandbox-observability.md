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
    workspace_id
      workspace state
      resource usage
      active commands
      recent request and async method chains
  ```

- Collect a current snapshot for each sandbox and each active workspace.
- Track CPU, memory, disk, cgroup, time, and command activity when the data is
  available.
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
- `sandbox-manager` creates a per-sandbox runtime directory:

  ```text
  <runtime_root>/<sandbox_id>/
    runtime.sock
    runtime.pid
  ```

- `sandbox-daemon::ServerConfig` already carries `socket_path`, `pid_path`, and
  optional `sandbox_id`.
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
- Command active/completed state is owned by `CommandProcessStore`.
- Cgroup CPU/memory accounting is not currently present in `sandbox-runtime`.
  It must be added as an optional read-only sampler, not assumed to exist.

## Components

### 1. Daemon Observability Service

Each sandbox daemon owns a local `DaemonObservabilityService`.

Responsibilities:

- Know the current `sandbox_id`.
- Derive the local observability directory.
- Own the two SQLite stores.
- Collect daemon-local sandbox snapshots.
- Record method traces for requests handled by that daemon.
- Record linked async traces for later lifecycle work.
- Expose a daemon API such as `get_observability_snapshot`.

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

- Never inspect workspace internals directly.
- Treat unavailable daemons as unavailable sandbox snapshots, not as empty
  sandboxes.

The manager may cache the aggregate in the future, but the first version should
use daemon-local stores as the source of truth.

### 3. Snapshot Collectors

Snapshot collectors read current state from existing service owners:

- `WorkspaceStateSampler`: active workspace sessions and remount state.
- `CommandStateSampler`: active and recently completed commands.
- `ResourceSampler`: CPU, memory, cgroup, disk, and time.
- `SandboxStateSampler`: daemon-level identity, runtime roots, and health.

Collectors are pull-based. They should not run a high-frequency background
monitor in the first version.

### 4. Method Trace Context

`OperationTrace` is a lightweight request-local context.

Responsibilities:

- Hold `trace_id`, `request_id`, `sandbox_id`, `operation`, and optional
  `workspace_id` / `command_session_id`.
- Maintain a parent stack for method spans.
- Create method spans with `enter` and `measure`.
- Finish spans on early return, panic unwind, or normal return.
- Hand completed traces to the method trace writer.
- Create linked async traces when later background work starts.

### 5. SQLite Writers

Each daemon owns two bounded SQLite databases:

```text
<runtime_root>/<sandbox_id>/observability/
  method-trace.sqlite
  sandbox-state.sqlite
```

SQLite journal/WAL files may also appear beside these files.

Writes must be non-critical:

- A failed observability write must not fail the user request.
- Trace writes should happen after response projection or through a bounded
  writer queue.
- State sample writes should be rate-limited.
- The writer may drop observability records under pressure.

## Local Disk Layout

The daemon derives its observability directory from the parent of
`ServerConfig.socket_path`:

```text
socket_path = <runtime_root>/<sandbox_id>/runtime.sock
observability_dir = <runtime_root>/<sandbox_id>/observability
```

Example:

```text
/tmp/eos-daemons/container-1/
  runtime.sock
  runtime.pid
  observability/
    method-trace.sqlite
    sandbox-state.sqlite
```

Do not store observability data inside:

- a workspace upperdir;
- command scratch directories;
- `transcript.log`;
- layerstack storage.

Those paths have functional lifecycle semantics and may be destroyed, captured,
or published independently of observability.

## SQLite Store 1: Method Trace

File:

```text
<runtime_root>/<sandbox_id>/observability/method-trace.sqlite
```

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

## SQLite Store 2: Sandbox State

File:

```text
<runtime_root>/<sandbox_id>/observability/sandbox-state.sqlite
```

Purpose:

- Current sandbox snapshot.
- Current workspace snapshots.
- Recent resource samples.
- Current active command snapshots.

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
  runtime_dir TEXT,
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
  workspace_id TEXT,
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

CREATE TABLE IF NOT EXISTS command_snapshots (
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  command_session_id TEXT NOT NULL,
  lifecycle_state TEXT NOT NULL,   -- running | quiesced_for_remount | finalizing | cancelled
  finalization_state TEXT NOT NULL,
  workspace_ownership TEXT NOT NULL,
  started_at_unix_ms INTEGER,
  wall_time_ms REAL,
  command_total_time_ms REAL,
  process_group_id INTEGER,
  transcript_path TEXT,
  sampled_at_unix_ms INTEGER NOT NULL,
  PRIMARY KEY(sandbox_id, command_session_id)
);

CREATE INDEX IF NOT EXISTS idx_workspace_snapshots_sandbox
  ON workspace_snapshots(sandbox_id, workspace_id);

CREATE INDEX IF NOT EXISTS idx_resource_samples_workspace_time
  ON resource_samples(sandbox_id, workspace_id, sampled_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_command_snapshots_workspace
  ON command_snapshots(sandbox_id, workspace_id);
```

Retention:

- `sandbox_snapshots`, `workspace_snapshots`, and `command_snapshots` represent
  current state and are updated in place.
- `resource_samples` is time-series data and should be retained by time window.
- Suggested default: retain the last 30 minutes of resource samples per sandbox.
- Disk samples may be sampled less often than cgroup samples.

## Snapshot DTOs

The daemon API should expose typed DTOs. SQLite is an implementation detail.

```rust
pub struct SandboxSnapshot {
    pub sandbox_id: String,
    pub state: SandboxStateView,
    pub sampled_at_unix_ms: i64,
    pub runtime_dir: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub workspaces: Vec<WorkspaceSnapshot>,
}

pub struct WorkspaceSnapshot {
    pub workspace_id: String,
    pub state: WorkspaceStateView,
    pub profile: Option<String>,
    pub base_revision: Option<BaseRevisionView>,
    pub resources: ResourceSnapshot,
    pub active_commands: Vec<CommandSnapshot>,
    pub recent_traces: Vec<TraceSummary>,
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
  workspace_id
    state
    resources
    active commands
    recent traces
```

## Resource Sampling

### Cgroups

Cgroup support is optional in the first implementation.

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

Create the trace context at the operation boundary.

```rust
let mut trace = OperationTrace::new_request(
    request.request_id.clone(),
    request.op.clone(),
    sandbox_id.clone(),
);
```

Then pass `&mut trace` through operation dispatch and service calls:

```rust
entry.dispatch(operations, request, &mut trace)
```

Every call to `trace.enter(...)` or `trace.measure(...)` writes into the same
trace context, so each span automatically inherits:

```text
trace_id
request_id
sandbox_id
operation
parent_span_id
```

When a method discovers a workspace or command id, update the context:

```rust
trace.set_workspace_id(workspace.workspace_session_id.clone());
trace.set_command_session_id(command_session_id.clone());
```

This lets later spans and the completed trace be shown under:

```text
sandbox_id
  workspace_id
    command_session_id
```

## `enter` vs `measure`

Use `enter` for scopes and parent methods.

Use `measure` for one call or expression.

Rules:

- Public operation dispatch methods: `enter`.
- Public/service methods: `enter`.
- Important subcalls inside a service method: `measure`.
- Tiny helpers: no span.
- Loops: one span around the loop, not one span per item.
- Async task entrypoints: `enter` with link metadata.
- Cleanup/finalization paths: `enter`.

Example:

```rust
pub fn exec_command(
    &self,
    input: ExecCommandInput,
    trace: &mut OperationTrace,
) -> Result<CommandYield, CommandServiceError> {
    let _span = trace.enter("CommandOperationService::exec_command");

    if input.cmd.trim().is_empty() {
        return Err(CommandServiceError::InvalidCommand {
            message: "cmd must be non-empty".to_owned(),
        });
    }

    self.exec_validated_command(input, trace)
}
```

Example subcall:

```rust
let workspace = trace.measure("resolve_exec_workspace", || {
    self.resolve_exec_workspace(&input)
})?;
```

## Future Operation Authoring Rule

A new operation should get root timing for free.

The dispatcher creates:

```text
dispatch_operation
  <operation>::dispatch
```

The operation author should add spans only at method boundaries that matter.

Minimum expected effort for a new operation:

```rust
pub fn new_operation(
    &self,
    input: NewOperationInput,
    trace: &mut OperationTrace,
) -> Result<NewOperationOutput, NewOperationError> {
    let _span = trace.enter("NewOperationService::new_operation");

    let target = trace.measure("resolve_target", || {
        self.resolve_target(&input)
    })?;

    let output = trace.measure("execute_route", || {
        self.execute_route(target, trace)
    })?;

    Ok(output)
}
```

If the call goes through a different route or service, pass the same trace
context:

```rust
other_route.execute(input, trace)
```

If the route author adds no spans, operation-level timing still exists. If they
add a few spans, the UI gets a readable method chain.

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

Suggested first-pass spans:

```text
dispatch_operation
exec_command::dispatch
parse_input
CommandOperationService::exec_command
resolve_exec_workspace
command_admission
start_command_process
register_active_command
start_completion_watcher
initial_exec_yield
command_yield_response
```

Do not instrument every small helper on the first pass.

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
origin_request_id
```

The UI should render the async chain under the same workspace and command as
the original request.

## Request and Async Trace IDs

Request trace id:

```text
trace_id = "request:" + request_id
```

Async trace id:

```text
trace_id = "async:" + async_name + ":" + command_session_id
```

If there can be multiple async traces for the same command and async name, add a
monotonic sequence:

```text
async:command_finalization:cmd_1:1
```

Span ids should be trace-local monotonic ids or globally unique strings:

```text
span_id = trace_id + ":" + call_index
```

## Active vs Completed Traces

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

The daemon should collect snapshots on demand and optionally on a timer.

Recommended behavior:

- Fast state fields: collect every request for snapshot API.
- Cgroup CPU/memory: collect at most once per second per workspace.
- Disk upperdir stats: collect at most once every 10 to 30 seconds per
  workspace.
- Method traces: record on request and async completion.

Snapshot calls should return cached samples when a sampler is rate-limited.

## Failure Policy

Observability must be best effort.

- If a sampler fails, include the error in that sampler's fields.
- If SQLite is locked, retry briefly and then drop the record.
- If the writer queue is full, drop the oldest unflushed observability item.
- If `method-trace.sqlite` fails, continue updating `sandbox-state.sqlite`.
- If `sandbox-state.sqlite` fails, continue recording method traces.
- Never fail `exec_command`, `write_command_stdin`, `read_command_lines`,
  `squash`, or workspace lifecycle work because observability failed.

## Prometheus, Grafana, and Loki

First version:

- No Prometheus requirement.
- No Grafana requirement.
- No Loki requirement.
- No OTLP requirement.

Future optional integrations:

- Prometheus can scrape numeric gauges/counters derived from
  `sandbox-state.sqlite`.
- Grafana can visualize Prometheus metrics and possibly query SQLite through an
  adapter if needed.
- Loki should remain out of this design unless the project explicitly wants
  searchable logs again.

Prometheus is not a good storage format for method call chains. Keep method
chains in `method-trace.sqlite`.

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

- Add observability DTOs.
- Add `DaemonObservabilityService`.
- Create `method-trace.sqlite`.
- Create `sandbox-state.sqlite`.
- Add best-effort SQLite writer.

### Phase 2: Runtime Snapshots

- Snapshot active workspaces from `WorkspaceSessionService`.
- Snapshot active commands from `CommandProcessStore`.
- Add disk stats using existing upperdir tree stats.
- Add cgroup sampler with unavailable state if paths are absent.

### Phase 3: Request Method Traces

- Create `OperationTrace` at daemon/runtime dispatch.
- Add automatic root dispatch span.
- Add spans for `exec_command`, `write_command_stdin`, `read_command_lines`,
  and `squash`.
- Persist completed request traces and spans.

### Phase 4: Async Method Traces

- Add linked traces for command finalization.
- Link by `origin_request_id`, `command_session_id`, and `workspace_id`.
- Add linked traces for workspace destroy/remount if those run outside the
  original request in future code.

### Phase 5: Manager Aggregation

- Add daemon API for observability snapshot.
- Add manager aggregation across ready sandboxes.
- Render hierarchy:

  ```text
  sandbox_id
    workspace_id
  ```

### Phase 6: Optional Metrics Export

- Add Prometheus export only after the local snapshot and trace model are
  stable.
- Export aggregate numeric metrics only.
- Keep method traces in SQLite.

## Verification

Focused checks after implementation should include:

```sh
cargo fmt --check -p sandbox-runtime -p sandbox-daemon -p sandbox-manager
cargo check -p sandbox-runtime -p sandbox-daemon -p sandbox-manager --tests
cargo test -p sandbox-runtime exec_command
cargo test -p sandbox-runtime workspace_session
cargo test -p sandbox-daemon dispatch
cargo test -p sandbox-manager manager_core
```

Storage-specific tests should verify:

- SQLite schema migration creates both databases.
- A request trace maps all spans to the same `request_id`.
- An async command finalization trace links to `origin_request_id`.
- A snapshot with missing cgroup paths records unavailable state.
- Disk sampler rate limiting returns cached data.
- Observability writer failure does not fail a runtime operation.
