# Phase 4.5: Namespace Execution Store and Traces

## Purpose

Phase 4.5 introduces a minimal runtime ledger for shell work executed through
the workspace namespace runner. The root concept is a namespace execution
attempt, identified by `namespace_execution_id`, not a command session and not a
runner-specific trace report.

The first implementation records parent-observed namespace execution lifecycle
data:

- which `workspace_session_id` namespace capability was used;
- which operation launched shell execution;
- the stable namespace execution id;
- lifecycle status, timing, and bounded errors.

Child-produced runner method spans are deferred. The first pass should not
extend `RunResult`, should not add a child-produced `NamespaceRunnerTraceReport`,
and should not model commands as runner owners.

## Live Boundary

The live hierarchy is:

```text
WorkspaceSession
  WorkspaceEntry namespace capability
  optional CommandSession
  namespace-runner child process attempts
```

`WorkspaceSession` owns the namespace capability exposed as `WorkspaceEntry`.
Command execution consumes that capability to run shell work.

Workspace mount and remount may use the same runner substrate, but Phase 4.5
does not model them as namespace executions. They stay workspace lifecycle work
until a separate lifecycle trace shape is justified. Do not add them as
namespace execution kinds just because they share process plumbing.

The unified model is:

```text
WorkspaceSession
  NamespaceExecutionAttempt
    namespace_execution_id
    operation_name
```

For command execution:

```text
CommandProcessStore
  command_session_id
  namespace_execution_id

NamespaceExecutionStore
  namespace_execution_id
  operation_name = exec_command
```

The generic namespace execution record does not have a dedicated
`command_session_id` field. Command identity stays in the command domain. When
command code needs correlation, the command record carries both
`command_session_id` and `namespace_execution_id`; the namespace execution
record does not carry the command id back.

## Namespace Execution Store

Add a runtime-side `NamespaceExecutionStore`. It is a state ledger, not a SQLite
store and not an observability service. It should live with the runtime services
that own workspace sessions and command execution state, and it should be
usable by future shell namespace operations without gaining operation-specific
ids.

The record shape is defined once in the implementation-shape section below.

Do not add a second operation id to `NamespaceExecutionRecord`. The stable
identifier for this ledger is `namespace_execution_id`. If another domain needs
correlation, that domain stores `namespace_execution_id`.

`request_id` means the external runtime request id from
`sandbox_protocol::Request`. Do not populate it from
`NamespaceRunnerRequest.request_id`; the runner DTO field is functional protocol
and may contain command-local or internally generated values.

The store should expose narrow operations:

```text
allocate_namespace_execution_id() -> NamespaceExecutionId
begin_namespace_execution(namespace_execution_id, record metadata)
mark_namespace_execution_running(namespace_execution_id)
complete_namespace_execution(namespace_execution_id, terminal_status, exit_code, bounded_error)
snapshot_active_namespace_executions()
drain_completed_namespace_executions(cursor/limit)
ack_completed_namespace_executions(namespace_execution_ids)
```

The exact retention window can be small, but completed records are not eligible
for normal window eviction until the daemon acknowledges a successful completed
trace projection. The purpose is to support live observability and completed
trace projection, not to become durable command history.

Store update failures are observability failures, not user operation failures.
Namespace execution id allocation must be infallible after the process has been
admitted. If `begin`, `mark_running`, `complete`, or projection acknowledgement
cannot update the store, the command path should continue and the daemon should
surface a bounded partial error. Command records still keep the allocated
`namespace_execution_id` even when a store mutation fails.

Store mutation should be atomic at the namespace execution level. Completing an
execution moves the record from active to pending projection under one store
lock; there must not be an observable gap where the record is neither active nor
available for projection. Duplicate completion is a no-op that returns or
preserves the first terminal record; it must not create a second completed record
or leave an active record behind.

## Implementation Shape

Keep the runtime implementation in the operation crate. Do not add a new crate
or a runner-child module.

```text
crates/sandbox-runtime/operation/src/
  namespace_execution.rs
  command/service/process_store.rs
  command/service/impls/exec_command.rs
  services.rs
  observability.rs
```

`namespace_execution.rs` owns only the namespace execution ledger types and
store:

```rust
pub struct NamespaceExecutionId(pub String);

pub struct NamespaceExecutionStore {
    inner: Mutex<NamespaceExecutionState>,
    next_id: AtomicU64,
}

struct NamespaceExecutionState {
    active: HashMap<NamespaceExecutionId, NamespaceExecutionRecord>,
    pending_projection: VecDeque<NamespaceExecutionRecord>,
    recent_projected: VecDeque<NamespaceExecutionRecord>,
}

pub struct NamespaceExecutionRecord {
    pub namespace_execution_id: NamespaceExecutionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub operation_name: String,
    pub request_id: Option<String>,
    pub lifecycle_state: NamespaceExecutionLifecycle,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub terminal_status: Option<NamespaceExecutionTerminalStatus>,
    pub exit_code: Option<i64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

pub enum NamespaceExecutionLifecycle {
    Starting,
    Running,
    Terminal,
}

pub enum NamespaceExecutionTerminalStatus {
    Ok,
    Error,
    TimedOut,
    Cancelled,
}
```

`terminal_status`, `exit_code`, `finished_at_unix_ms`, and `duration_ms` are
unset while the lifecycle is `Starting` or `Running`. A terminal record must set
`lifecycle_state = Terminal` and a concrete `terminal_status`.

`CommandProcessStore` remains command-owned. Its active command record adds only
the bridge id:

```rust
pub(crate) namespace_execution_id: NamespaceExecutionId,
```

`SandboxRuntimeOperations` should share one `NamespaceExecutionStore` with
command execution and observability snapshot collection:

```rust
namespace_execution: Arc<NamespaceExecutionStore>,
```

The same store `Arc` must be passed into `CommandOperationService` and used by
runtime observability snapshot collection. `SandboxRuntimeOperations::new`
should assert that command execution and snapshot collection share the same
namespace execution store, mirroring the existing workspace-session sharing
check. Expose snapshot/projection methods instead of requiring callers to mutate
the store field directly.

Do not add a second module tree until the first implementation proves this file
is carrying unrelated responsibilities.

## Execution Scope

Phase 4.5 records shell namespace execution only:

```text
shell_exec
```

This value describes the scope of the phase, not a field on
`NamespaceExecutionRecord`. A single-value enum or string field would add shape
without adding meaning.

Phase 4.6 keeps this decision and deletes the older command-shaped active
execution lane instead of adding a replacement kind, runner, or scope axis.

Do not add a `NamespaceRunnerMode` trace field. If implementation needs an
internal enum for dispatch, keep it in the runner/adapter layer. Mount/remount
runner modes remain out of the namespace execution store.

## Command Integration

Command execution keeps command-specific state in `CommandProcessStore`:

```text
command_session_id
workspace_session_id
namespace_execution_id
transcript_path
command lifecycle/finalization
```

The command store owns command lifecycle, transcript lookup, stdin/stdout
operations, and finalization. It does not own the generic namespace execution
model.

When `exec_command` starts a command:

1. Resolve or create the `WorkspaceSession`.
2. Allocate `command_session_id` in the command store.
3. Begin a namespace execution:

   ```text
   operation_name = exec_command
   workspace_session_id = resolved workspace_session_id
   request_id = request id when one exists
   ```

4. Store `namespace_execution_id` on the active command record.
5. Spawn the runner using the existing command process path.
6. Mark the namespace execution running after the parent confirms child start.
7. Complete the namespace execution when the runner/command process reaches a
   terminal state.

If spawn, start acknowledgement, or request-payload handoff fails after
`namespace_execution_id` allocation, complete the namespace execution with
`terminal_status = Error` and a bounded error. Completion must be idempotent: a
second terminal observation for the same `namespace_execution_id` must not create
a second completed record or leave an active record behind.

Command APIs continue to use `command_session_id` for `write_command_stdin`,
`read_command_lines`, cancellation, finalization, and transcript lookup. The
generic namespace execution store does not need a `command_session_id` column or
field.

## Future Operation Integration

If a later operation uses namespace shell execution, it should use the same store
without becoming a command operation:

```text
operation_name = concrete operation name
workspace_session_id = resolved workspace_session_id
request_id = request id when one exists
```

Some namespace executions are not launched by a public CLI/runtime request. In
that case `request_id` is `None`; the execution still has
`namespace_execution_id`, `workspace_session_id`, and `operation_name`.

Do not add enum variants or operation-specific id fields for every future
operation. New shell operations should set `operation_name`; their own domain
records may store `namespace_execution_id` if they need a reverse lookup.

## Child-Visible Data

The first implementation should not send observability trace context to the
child. Parent-observed lifecycle timing is enough to make namespace execution
visible and generic.

If a later phase adds child-produced runner spans, the child-visible data should
be limited to:

```rust
pub struct NamespaceExecutionContext {
    pub namespace_execution_id: String,
}
```

Do not include these fields in child-visible data:

- `command_session_id`;
- `workspace_session_id`;
- `request_id`;
- operation owner enums;
- shell command text;
- environment dumps;
- SQLite paths, writer handles, daemon stores, or `sandbox-observability` types.

## Transport

Do not extend `RunResult` with observability data in Phase 4.5.

The existing runner result is functional protocol:

```rust
pub struct RunResult {
    pub exit_code: i32,
    pub payload: serde_json::Value,
}
```

It is consumed by command execution for terminal status. Other runner modes may
also use `RunResult` for their own functional payloads, but those modes are not
part of the Phase 4.5 namespace execution ledger.

Phase 4.5 transport is parent-side store updates:

```text
parent allocates namespace_execution_id -> NamespaceExecutionStore::begin
parent confirms child start -> NamespaceExecutionStore::mark_running
parent observes terminal state -> NamespaceExecutionStore::complete
daemon collector/projector reads runtime store -> observability rows
```

If child-produced spans are later required, use a separate bounded control pipe
or an internal parent envelope beside `RunResult`. Do not write trace data into
`transcript.log`, do not write directly to `observability.sqlite`, and do not
let missing or malformed child trace data fail the user operation.

## Persistence

Persist namespace execution observability outside the child process through
daemon-owned mapping code.

The runtime store is the source for active and recently completed namespace
execution facts. Daemon observability projects active facts into a namespace
execution snapshot table and drains completed facts into completed trace rows.
Do not project namespace executions into the current command-shaped execution
snapshot row shape.

Phase 4.5 requires an explicit observability schema migration before persistence:

- add a `namespace_execution_snapshots` table keyed by
  `(sandbox_id, namespace_execution_id)`;
- add completed-trace storage support for `namespace_execution_id` and
  `workspace_session_id`, either by extending the trace schema or by adding a
  namespace-execution trace table;
- keep existing command snapshot and command trace rows readable without
  rewriting them as namespace execution rows.

The migration must not require namespace execution rows to fill command-shaped
snapshot fields such as command identity, command text, finalization state,
process group id, transcript path, or workspace ownership. If the existing
storage API still exposes older generic execution field names, Phase 4.5 should
add a typed namespace projection API instead of aliasing those names into
namespace execution records.

Recommended completed trace row shape:

```text
trace_id = "namespace_execution:" + namespace_execution_id
kind = "namespace_execution"
operation = operation_name
request_id = request_id, if known
namespace_execution_id = namespace_execution_id
workspace_session_id = workspace_session_id
status = ok | error | timed_out | cancelled
exit_code = exit_code, if known
started_at_unix_ms = started_at_unix_ms
finished_at_unix_ms = finished_at_unix_ms
duration_ms = duration_ms
error_kind = bounded error kind, if any
error_message = bounded error message, if any
```

Do not store command identity in namespace execution rows. Do not add a parallel
workspace identifier alias. If the current storage schema only has a differently
named workspace column, Phase 4.5 should migrate or adapt that projection to use
`workspace_session_id` for these rows rather than carrying two names for the
same value.

Map completed trace status from the terminal shell result, not from lifecycle
alone. A zero-exit shell result becomes `ok`; a nonzero shell result becomes
`error`; timeout becomes `timed_out`; cancellation becomes `cancelled`.
`Starting` and `Running` are snapshot-only states; they should not produce
completed trace rows until the namespace execution reaches a terminal state.

Daemon projection must drain completed records from the runtime store, write the
completed trace rows, and acknowledge only the ids whose writes succeeded.
Unacknowledged completed records remain pending projection until the configured
retention ceiling is hit; if the ceiling is hit, the daemon must surface a
bounded partial error naming the dropped namespace execution ids.

`trace_links` is deferred. A namespace execution has one
`namespace_execution_id` in the first implementation. Add a link table only when
query APIs prove that one namespace execution needs multiple independent links.

## Storage and Snapshot Shape

Active namespace executions feed `namespace_execution_snapshots`. The snapshot
row should be generic and minimal:

```text
namespace_execution_id = namespace_execution_id
operation = operation_name
workspace_session_id = workspace_session_id
lifecycle_state = starting | running
sampled_at_unix_ms = sampled_at_unix_ms
```

Do not add `command_session_id` to namespace execution snapshots. Phase 4.6
deletes the older command execution snapshot lane instead of aliasing it into
namespace execution rows. The namespace execution snapshot is the only active
execution snapshot row.

Do not add command text, transcript path, process group id, workspace ownership,
or finalization state to namespace execution snapshots. Those belong to command
or workspace lifecycle snapshots.

Terminal records are not kept as active namespace execution snapshots. Their
observable terminal form is the completed namespace execution trace row after
successful projection.

## Span Boundaries

Phase 4.5 first pass records a single parent-observed namespace execution trace
or snapshot lifecycle, not child method spans.

Deferred child span candidates are:

```text
namespace_execution
  runner::run
  run_setns
  shell_exec::execute_shell
  wait_for_command_execution_scope
```

Do not record one span per wait-loop iteration, environment variable, shell
output chunk, transcript line, or filesystem entry.

## Non-Goals

Phase 4.5 does not implement:

- command-owned runner traces;
- `NamespaceRunnerOwner`;
- `NamespaceRunnerMode` as trace metadata;
- `command_session_id` in `NamespaceExecutionRecord`;
- `command_session_id` in child-produced data;
- operation-specific id fields in `NamespaceExecutionRecord`;
- workspace identifier aliases beside `workspace_session_id`;
- workspace mount/remount rows in the namespace execution store;
- `runner_trace` on `RunResult`;
- `NamespaceRunnerTraceReport` in the first pass;
- direct SQLite writes from the runner child process;
- command output, transcript, stdin, environment, or shell text ingestion;
- response envelope changes;
- manager aggregation or public query APIs;
- `trace_links`.

## Verification

Focused checks after implementation should include:

```sh
cargo fmt --check
cargo check -p sandbox-runtime --tests
cargo check -p sandbox-runtime-command --tests
cargo check -p sandbox-runtime-namespace-process --tests
cargo check -p sandbox-daemon --tests
cargo test -p sandbox-daemon observability
```

If Linux-only runner code changes, also run:

```sh
cargo check --tests --target x86_64-unknown-linux-gnu
```

Required behavior coverage:

- namespace execution ids are generated parent-side;
- namespace execution id allocation succeeds independently from store mutation;
- command records retain `namespace_execution_id` even when a namespace store
  update fails;
- command execution and runtime observability share the same
  `NamespaceExecutionStore` instance;
- command active records keep `command_session_id` and `namespace_execution_id`;
- `NamespaceExecutionRecord` has no dedicated `command_session_id`;
- `NamespaceExecutionRecord` has no operation-specific id field;
- `NamespaceExecutionRecord.request_id` is optional;
- namespace store completion atomically moves a record out of active state and
  into pending projection;
- completion is idempotent for duplicate background, polling, cancellation, and
  timeout observations;
- completed records are drained and acknowledged only after successful daemon
  projection;
- failed projection leaves completed records pending until a later successful
  projection or a bounded retention-drop partial error;
- namespace execution rows use `workspace_session_id` and do not carry a second
  workspace identifier alias;
- namespace execution trace rows include status, timing, and bounded error
  fields;
- zero-exit shell completion maps to `ok`;
- nonzero shell completion maps to `error`;
- timeout maps to `timed_out`;
- cancellation maps to `cancelled`;
- `request_id` comes only from the external runtime request, not the runner DTO;
- spawn/start failures complete the namespace execution with terminal error
  status, and observability persistence failure does not fail the user operation;
- duplicate completion does not create duplicate completed records or leak an
  active record;
- schema tests cover `namespace_execution_snapshots`, completed namespace trace
  projection, migration idempotency, and checksum drift;
- daemon projection tests prove namespace execution snapshots are not written
  through the command-shaped execution snapshot adapter;
- workspace mount/remount do not create namespace execution records in Phase
  4.5;
- `RunResult` stays functional and has no `runner_trace`;
- missing observability store/projector failures do not fail user operations;
- no command output, transcript text, shell text, environment dump, or per-loop
  wait events appear in namespace execution records or trace rows;
- the runner child never opens or writes `observability.sqlite`.

Required guard checks after implementation:

```sh
rg -n 'operation_execution_id|NamespaceExecutionKind|mount_overlay|remount_overlay|runner_trace|NamespaceRunnerTraceReport' crates/sandbox-runtime/operation/src/namespace_execution.rs crates/sandbox-daemon/src/observability/namespace_execution.rs crates/sandbox-observability/src/namespace_execution.rs
rg -n 'workspace_id|execution_kind' crates/sandbox-runtime/operation/src/namespace_execution.rs crates/sandbox-daemon/src/observability/namespace_execution.rs crates/sandbox-observability/src/namespace_execution.rs
```

The first command should return no Phase 4.5 namespace execution implementation
hits. The second command should return no namespace execution projection hits;
if implementation files are named differently, run the same patterns over the
actual namespace execution store and projection files. Older command, workspace
snapshot, or resource-sample code may still carry their existing storage fields
until a separate schema cleanup removes them.
