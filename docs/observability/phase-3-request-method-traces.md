# Phase 3 Request Method Traces

Status: draft implementation spec

## Parent Spec

[sandbox-observability.md](./sandbox-observability.md)

## Builds On

- [phase-1-observability-foundation.md](./phase-1-observability-foundation.md)
- [phase-2-runtime-snapshots.md](./phase-2-runtime-snapshots.md)

## Exact Goal

Phase 3 adds completed request method traces for the daemon-owned runtime
operation path. It records one request-local runtime span tree, a root
`dispatch_operation` span, a matched `<operation>::dispatch` span, and at most
one public service-method span for each selected operation.

Phase 3 implements only:

- create one request-local `OperationTrace` at daemon dispatch only when daemon
  observability is enabled;
- pass an optional trace context into `sandbox_runtime::dispatch_operation`;
- add the automatic root `dispatch_operation` span;
- add one public service-method span for `exec_command`,
  `write_command_stdin`, `read_command_lines`, and `squash`;
- persist completed request traces and spans through daemon-owned
  observability storage;
- preserve the current `sandbox_protocol::Response` payload shape.

Phase 3 must not add response envelopes, async trace links, runner child-process
traces, manager aggregation, query APIs, external observability services, command
transcript ingestion, decode-error traces, trace hierarchy columns or indexes,
runtime SQLite writes, or a `sandbox-observability` dependency from
`sandbox-runtime`.

## Current Repo Grounding

This section describes the live checkout this spec is grounded in. Docs are
treated as rollout proposals; live code is the source of truth.

### Phase 2 Snapshot State

Phase 2 snapshot code is present in live code even though
`docs/observability/phase-2-runtime-snapshots.md` still has an unchecked
completion checklist.

Live Phase 2 pieces:

- `crates/sandbox-runtime/operation/src/observability.rs` defines
  `RuntimeObservabilitySnapshot`, `RuntimeWorkspaceSnapshot`, and namespace
  execution snapshot types.
- `crates/sandbox-runtime/operation/src/services.rs` exposes
  `SandboxRuntimeOperations::observability_snapshot`.
- `crates/sandbox-runtime/operation/src/workspace_session/service/snapshot.rs`
  snapshots active workspace sessions.
- `crates/sandbox-runtime/operation/src/namespace_execution.rs` snapshots active
  namespace executions.
- `crates/sandbox-daemon/src/observability/service.rs` defines
  `DaemonObservability` and writes sandbox, workspace, namespace execution, and
  resource snapshot records.
- `crates/sandbox-observability/src/store.rs` has two migrations:
  `phase_1_observability_foundation` and `phase_2_runtime_snapshots`, with
  later migrations adding namespace execution tables and dropping the old
  `execution_snapshots` table from the final schema.
- `crates/sandbox-daemon/tests/unit/observability.rs`,
  `crates/sandbox-runtime/operation/tests/observability_snapshot.rs`, and
  `crates/sandbox-observability/tests/schema.rs` cover the current storage and
  snapshot behavior.

Still incomplete or dirty pieces relevant to Phase 3:

- The Phase 2 spec checklist is not updated to match live code.
- Active command work is represented as namespace execution snapshots; Phase 3
  request trace timing must not depend on command-shaped active snapshot fields.
- Cgroup sampling is currently unavailable-only: the daemon writes
  `cgroup_available = 0` when no explicit daemon-owned cgroup target exists, and
  live code does not yet read cgroup v2 files. This does not block request
  traces.
- Daemon snapshot collection errors are ignored by
  `SandboxDaemonServer::trigger_observability_collection`; Phase 3 trace
  persistence must follow the same best-effort request-safety rule.

### Current Daemon Server Shape

`crates/sandbox-daemon/src/server/runtime.rs` currently defines:

```rust
pub struct SandboxDaemonServer {
    pub(crate) config: ServerConfig,
    pub(crate) operations: Arc<SandboxRuntimeOperations>,
    pub(crate) observability: Option<Arc<DaemonObservability>>,
    pub(crate) shutdown: CancellationToken,
}
```

`ServerConfig` carries `socket_path`, `pid_path`, optional TCP fields,
`auth_token`, and optional `sandbox_id`.

`SandboxDaemonServer::new` currently creates
`DaemonObservability::from_config(&config).map(Arc::new)`. Observability is
disabled when `sandbox_id` is missing, empty, path derivation fails, or the
store cannot open.

### Current Daemon Dispatch Flow

`crates/sandbox-daemon/src/server/dispatch.rs` currently:

- parses bytes into `serde_json::Value`;
- strips TCP auth before request decoding;
- decodes `sandbox_protocol::Request` through `decode_request`;
- validates daemon scope through `validate_daemon_scope`;
- calls `sandbox_runtime::dispatch_operation(&operations, &request)` inside
  `tokio::task::spawn_blocking`;
- projects the runtime `Response` with `into_json_value`;
- triggers Phase 2 snapshot collection only after `Ok(response)` from the
  blocking task.

Phase 3 must keep that response projection behavior. Trace persistence happens
after the operation response has been projected and must not change the response
value returned to the caller.

### Current Runtime Operation Boundary

`crates/sandbox-runtime/operation/src/operation.rs` currently defines:

```rust
pub(crate) struct OperationEntry {
    pub(crate) name: &'static str,
    pub(crate) cli: Option<&'static CliOperationSpec>,
    pub(crate) dispatch:
        fn(&SandboxRuntimeOperations, &sandbox_protocol::Request) -> sandbox_protocol::Response,
}
```

`OperationEntry::cli` accepts the same two-argument function pointer.

`dispatch_operation` currently has this shape:

```rust
pub(crate) fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response
```

`crates/sandbox-runtime/operation/src/lib.rs` re-exports it as:

```rust
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response
```

Phase 3 changes these signatures to pass a runtime-owned trace context.

### Current Public Runtime Operation Entries

The current operation entry groups are:

- `crates/sandbox-runtime/operation/src/command/service/impls/mod.rs`
  - `exec_command`
  - `write_command_stdin`
  - `read_command_lines`
- `crates/sandbox-runtime/operation/src/layerstack/service/impls/mod.rs`
  - `squash`

The current dispatch functions are:

```rust
pub(crate) fn dispatch(operations: &SandboxRuntimeOperations, request: &Request) -> Response
```

for each command operation, and:

```rust
pub(crate) fn dispatch(operations: &SandboxRuntimeOperations, _request: &Request) -> Response
```

for `squash`.

The current selected service methods are:

- `CommandOperationService::exec_command(input)`;
- `CommandOperationService::write_command_stdin(input)`;
- `CommandOperationService::read_command_lines(input)`;
- `LayerStackService::squash()`.

Phase 3 should change the selected methods directly to accept
`&OperationTrace`. Do not add alias methods, compatibility wrappers, or a second
parallel operation path.

### Current Phase 1/2 Store Shape

`crates/sandbox-observability/src/records.rs` currently defines
`TraceRecord` with:

```rust
pub struct TraceRecord {
    pub trace_id: String,
    pub kind: String,
    pub status: String,
    pub sandbox_id: String,
    pub operation: String,
    pub request_id: Option<String>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}
```

`SpanRecord` currently has `span_id`, `trace_id`, optional `parent_span_id`,
`method_name`, `call_index`, `status`, timing fields, and optional error fields.

`crates/sandbox-observability/src/store.rs` currently exposes:

```rust
pub fn insert_trace(
    &self,
    trace: &TraceRecord,
    spans: &[SpanRecord],
) -> Result<(), StoreError>
```

and inserts one trace row plus all span rows in a single SQLite transaction.

The current `traces` table does not have `workspace_id` or
`command_session_id`. Phase 3 intentionally does not add those hierarchy fields
or indexes; they belong with the first concrete daemon query API or a later
hierarchy-correlation phase.

### Current Request and Response Shape

`crates/sandbox-protocol/src/request.rs` defines:

```rust
pub struct Request {
    pub op: String,
    pub request_id: String,
    pub scope: CliOperationScope,
    pub args: Value,
}
```

`crates/sandbox-protocol/src/response.rs` defines `Response` as a private raw
`serde_json::Value` wrapper. `Response::ok(result)` and
`Response::running(result)` store the result directly. Fault responses are
top-level JSON objects with an `error` field. Phase 3 must not replace this
shape with `{ "result": ..., "meta": ... }`.

### Command Output and Transcripts

Command output remains in operation responses such as `CommandYield.output` and
`CommandLinesOutput.output`. Command transcript content remains in command
transcript artifacts. Phase 3 trace records and span records must store only
method names, parentage, ordering, status, timing, and bounded error metadata.
They must not ingest command output, transcript rows, stdout/stderr chunks,
environment dumps, or shell text.

## Non-Goals

Phase 3 does not implement:

- Phase 3.5 targeted deep request spans;
- Phase 4 async finalization traces;
- Phase 4.5 cross-process namespace-runner traces;
- `trace_links`;
- `origin_request_id`;
- `correlation_kind`;
- `correlation_id`;
- `async_name`;
- manager aggregation;
- daemon query APIs such as `get_observability_snapshot`;
- Prometheus, Grafana, Loki, Tempo, OTLP, or log export;
- command transcript ingestion;
- response envelopes such as `{ "result": ..., "meta": ... }`;
- `traces.workspace_id`, `traces.command_session_id`, or hierarchy indexes;
- daemon decode-error trace records;
- runtime SQLite writes;
- a `sandbox-observability` dependency from `sandbox-runtime`;
- a new crate;
- manager-side files.

## Architecture

### Runtime Trace Context

Add runtime-owned request span collection in
`crates/sandbox-runtime/operation/src/observability.rs`. Do not split the file
unless the final implementation becomes hard to read; the current snapshot DTOs
are small.

`OperationTrace` is a request-local span collector. It does not know
`sandbox_id`, `request_id`, operation names as trace metadata, SQLite, daemon
paths, `sandbox-observability` records, response JSON, command output, transcript
content, workspace hierarchy, or command hierarchy.

Use this domain split:

- `OperationTrace`: mutable request span collector used while dispatch runs.
- `CompletedOperationTrace`: immutable runtime DTO containing only trace timing
  and completed spans.
- `CompletedOperationSpan`: immutable runtime DTO for one completed span.

The runtime trace state must include only:

- monotonic request start time as `Instant`;
- Unix request start time in milliseconds;
- active parent stack as `call_index` values;
- completed spans;
- next stable `call_index`.

Runtime must not store:

- `trace_id`;
- `request_id`;
- `sandbox_id`;
- `operation`;
- `workspace_id`;
- `command_session_id`;
- terminal trace status;
- terminal trace error kind or message.

The daemon derives the request trace id as:

```text
trace_id = "request:" + request_id
```

The daemon maps span ids from call indexes:

```text
span_id = trace_id + ":span:" + call_index
```

Pass `Option<&OperationTrace>`, not a disabled/no-op trace object. `None` means
daemon observability was unavailable and runtime dispatch does not record spans.
This keeps the disabled path out of runtime state and avoids completed DTOs that
exist only to be ignored.

When a trace is present, pass `&OperationTrace`, not `&mut OperationTrace`.
Phase 3 needs RAII span guards that close on normal return, early return, and
panic unwind while still allowing nested calls to enter child spans. A guard that
holds `&mut OperationTrace` would borrow the trace for the full scope and make
nested instrumentation awkward or impossible without unsafe code or manual close
calls. `OperationTrace` should use interior mutability, such as
`RefCell<TraceState>`, because the trace is request-local and not shared across
concurrent threads. It does not need `Arc<Mutex<_>>`.

Expected API shape:

```rust
pub struct OperationTrace {
    state: RefCell<TraceState>,
}

pub struct SpanGuard<'a> {
    trace: &'a OperationTrace,
    call_index: i64,
}

impl OperationTrace {
    pub fn new() -> Self;

    pub fn enter(&self, method_name: &'static str) -> SpanGuard<'_>;

    pub fn measure<T>(
        &self,
        method_name: &'static str,
        call: impl FnOnce() -> T,
    ) -> T;

    pub fn complete(&self) -> CompletedOperationTrace;
}
```

`enter` creates a span, assigns `call_index`, uses the current top of the parent
stack as `parent_call_index`, pushes the new call index, and returns a
`SpanGuard`. Dropping the guard pops the span if it is still active and records
finish time and duration. If the guard is dropped during unwind, the span closes
with `status = "panic"`; otherwise it closes with `status = "ok"`.

`measure` is a convenience wrapper around `enter` for one call or expression.
Do not add `measure_result`. Phase 3 records operation errors by letting daemon
code inspect the projected response JSON once, after runtime dispatch returns.

The completed runtime DTO must be produced before daemon persistence. The daemon
maps it into `TraceRecord` and `SpanRecord`, injects `sandbox_id`, `request_id`,
`operation`, `kind = "request"`, response-derived status/error metadata, and the
storage span ids. This keeps runtime independent of `sandbox-observability`.

### Dispatch Boundary

Change the runtime dispatch boundary to accept `Option<&OperationTrace>`.

Expected `OperationEntry` shape in
`crates/sandbox-runtime/operation/src/operation.rs`:

```rust
use crate::observability::OperationTrace;

pub(crate) struct OperationEntry {
    pub(crate) name: &'static str,
    pub(crate) cli: Option<&'static CliOperationSpec>,
    pub(crate) dispatch: fn(
        &SandboxRuntimeOperations,
        &sandbox_protocol::Request,
        Option<&OperationTrace>,
    ) -> sandbox_protocol::Response,
}
```

Expected dispatch shape:

```rust
pub(crate) fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response {
    measure_optional(trace, "dispatch_operation", || {
        operation_entry_groups()
            .into_iter()
            .flat_map(|entries| entries.iter())
            .find(|entry| entry.name == request.op)
            .map_or_else(sandbox_protocol::Response::unknown_op, |entry| {
                measure_optional(trace, operation_dispatch_span(entry.name), || {
                    (entry.dispatch)(operations, request, trace)
                })
            })
    })
}
```

`operation_dispatch_span(entry.name)` should produce the exact operation
dispatch span name:

```text
exec_command::dispatch
write_command_stdin::dispatch
read_command_lines::dispatch
squash::dispatch
```

The public runtime entry in `crates/sandbox-runtime/operation/src/lib.rs`
changes to:

```rust
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response
```

The selected operation dispatch functions change to:

```rust
pub(crate) fn dispatch(
    operations: &SandboxRuntimeOperations,
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response
```

for command operations and `squash`. Only these dispatch wrappers receive the
optional trace. Do not change the public service methods
`CommandOperationService::exec_command`,
`CommandOperationService::write_command_stdin`,
`CommandOperationService::read_command_lines`, or `LayerStackService::squash`.
The dispatch wrappers should parse input normally, then wrap the single public
service method call in the selected service-method span.

Unknown operations still pass through `dispatch_operation`, so the request gets
at least:

```text
dispatch_operation
```

with terminal `unknown_op` metadata after response projection.

Request argument parse errors inside operation dispatch still get:

```text
dispatch_operation
<operation>::dispatch
```

because `<operation>::dispatch` wraps `parse_input`. Phase 3 must not add a
separate `parse_input` span.

Daemon request decode errors happen before a typed `Request` exists and are not
Phase 3 request-method traces. Do not add decode-error trace persistence in this
phase.

### Daemon Persistence

`SandboxDaemonServer::dispatch_request` owns trace creation and persistence.

Trace creation rule:

- if `self.observability.is_some()` and `self.config.sandbox_id` is a non-empty
  string, create `Some(OperationTrace::new())`;
- otherwise pass `None` into runtime dispatch and skip trace completion.

Do not create disabled/no-op traces. The daemon already owns the enabled/disabled
decision and can branch once at dispatch.

Expected daemon shape:

```rust
async fn dispatch_request(&self, request: Request) -> serde_json::Value {
    if let Err(response) = validate_daemon_scope(&request) {
        return response;
    }

    let trace = self.operation_trace_for();
    let trace_request_id = request.request_id.clone();
    let trace_operation = request.op.clone();
    let trace_sandbox_id = self
        .config
        .sandbox_id
        .as_ref()
        .filter(|sandbox_id| !sandbox_id.is_empty())
        .cloned();
    let observability = self.observability.clone();
    let operations = Arc::clone(&self.operations);
    let task = tokio::task::spawn_blocking(move || {
        let response = sandbox_runtime::dispatch_operation(
            &operations,
            &request,
            trace.as_ref(),
        );
        let value = response.into_json_value();
        if let (Some(observability), Some(completed_trace), Some(sandbox_id)) = (
            observability,
            trace.as_ref().map(OperationTrace::complete),
            trace_sandbox_id,
        ) {
            let _ = observability.insert_completed_operation_trace(
                sandbox_id,
                trace_request_id,
                trace_operation,
                &value,
                completed_trace,
            );
        }
        value
    });

    match task.await {
        Ok(response) => {
            self.trigger_observability_collection();
            response
        }
        Err(err) if err.is_cancelled() => { /* current cancelled response */ }
        Err(err) => { /* current internal-error response */ }
    }
}
```

`DaemonObservability::insert_completed_operation_trace` should:

- derive `trace_id` as `request:<request_id>`;
- inspect the projected response JSON once to derive trace status and bounded
  error metadata;
- map runtime trace DTOs into existing `TraceRecord` and `SpanRecord`;
- synthesize storage span ids from `trace_id` plus runtime call indexes;
- when the response is a fault, mark the deepest completed span with
  response-derived error metadata; this maps to the public service span for
  service errors, `<operation>::dispatch` for argument parse errors, and
  `dispatch_operation` for unknown operations;
- call `ObservabilityStore::insert_trace`;
- ignore write failures for the user operation response;
- optionally record bounded internal diagnostics later, but not in Phase 3
  response payloads.

Do not move SQLite handles, `ObservabilityStore`, `TraceRecord`, or
`SpanRecord` into `sandbox-runtime`.

Panic handling:

- `SpanGuard::drop` must close active spans during unwind.
- The daemon should keep the current user-facing panic behavior: a runtime panic
  maps to the existing internal daemon error path.
- Do not add `catch_unwind` in Phase 3. Persisting panic traces is deferred
  because it adds a second panic-handling path around the blocking task. Normal
  operation errors, unknown operations, and argument parse errors are the
  required Phase 3 persistence target.

Phase 2 snapshot collection continues after request handling. Trace persistence
should run before `trigger_observability_collection` so the completed method
trace is durable even if snapshot collection later fails.

### Storage Migration

No schema migration is required for Phase 3. The live Phase 1 schema already has
`traces`, `spans`, `TraceRecord`, `SpanRecord`, and
`ObservabilityStore::insert_trace`.

Do not add:

```text
traces.workspace_id
traces.command_session_id
idx_traces_workspace_time
idx_traces_command_time
trace_links
origin_request_id
correlation_kind
correlation_id
async_name
```

`workspace_id` and `command_session_id` are useful hierarchy fields, but adding
them now optimizes for future queries before Phase 3 has a query API. Reconsider
them with the first daemon-owned trace query API or an explicit hierarchy
correlation phase.

### Selected Span Policy

Phase 3 spans are inclusive timings. A span measures the full elapsed time of
the code block it wraps, including lower-level work that Phase 3 intentionally
does not split.

Every traced request starts with:

```text
dispatch_operation
<operation>::dispatch
```

`<operation>::dispatch` is omitted only when no operation entry matches the
request `op`.

For `exec_command`, add only:

```text
CommandOperationService::exec_command
```

This span wraps the existing `operations.command.exec_command(input)` call in
the operation dispatch wrapper. It includes validation, workspace resolution,
command process start, admission, watcher launch, and initial yield.

For `write_command_stdin`, add only:

```text
CommandOperationService::write_command_stdin
```

This span wraps the existing `operations.command.write_command_stdin(input)` call
in the operation dispatch wrapper. It includes the write-or-cancel branch and the
yield wait.

For `read_command_lines`, add only:

```text
CommandOperationService::read_command_lines
```

This span wraps the existing `operations.command.read_command_lines(input)` call
in the operation dispatch wrapper. It includes active and completed transcript
window reads, but trace records must not store transcript content.

For `squash`, add only:

```text
LayerStackService::squash
```

Explicitly defer these helper or lower-level spans:

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
WorkspaceSessionService::create_workspace_session
WorkspaceRuntimeService::create_workspace
layerstack snapshot or lease acquisition
CommandProcessSpawn::prepare
CommandProcess::spawn
build_namespace_runner_request
spawn_current_exe_ns_runner
runner::run
run_setns
shell_exec::execute_shell
wait_for_command_execution_scope
```

### Phase 3.5 / Phase 4 / Phase 4.5 Boundaries

Phase 3.5 may split proven-slow Phase 3 parent spans into a small number of
lower-level in-process spans. It must be justified by observed Phase 3 traces.
Phase 3.5 remains in-process and does not add async link metadata.

Phase 4 adds linked async traces for work that finishes after the request
returns, such as command finalization. It owns `origin_request_id`,
`correlation_kind`, `correlation_id`, and `async_name`.

Phase 4.5 adds cross-process namespace-runner traces. Runner internals such as
`runner::run`, `run_setns`, `shell_exec::execute_shell`, and
`wait_for_command_execution_scope` are not ordinary Phase 3 child spans because
they run across a process boundary and may outlive the request.

## Detailed File Plan

Keep the implementation small and use the existing crate layout.

Runtime files:

- `crates/sandbox-runtime/operation/src/observability.rs`
  - Keep the current snapshot DTOs in place.
  - Add the minimal `OperationTrace`, `SpanGuard`, `CompletedOperationTrace`, and
    `CompletedOperationSpan` types here unless the final implementation becomes
    hard to read.
- `crates/sandbox-runtime/operation/src/lib.rs`
  - Re-export `OperationTrace` and completed trace DTOs as needed by
    `sandbox-daemon`.
  - Change public `dispatch_operation` signature to accept
    `Option<&OperationTrace>`.
- `crates/sandbox-runtime/operation/src/operation.rs`
  - Change `OperationEntry` function pointer type.
  - Add root `dispatch_operation` span.
  - Add `<operation>::dispatch` span names.
- `crates/sandbox-runtime/operation/src/command/service/impls/mod.rs`
  - Update operation entry constants for the new dispatch function pointer.
- `crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs`
  - Add optional trace parameter to dispatch only.
  - Wrap only `operations.command.exec_command(input)` in
    `CommandOperationService::exec_command`.
- `crates/sandbox-runtime/operation/src/command/service/impls/write_command_stdin.rs`
  - Add optional trace parameter to dispatch only.
  - Wrap only `operations.command.write_command_stdin(input)` in
    `CommandOperationService::write_command_stdin`.
- `crates/sandbox-runtime/operation/src/command/service/impls/read_command_lines.rs`
  - Add optional trace parameter to dispatch only.
  - Wrap only `operations.command.read_command_lines(input)` in
    `CommandOperationService::read_command_lines`.
- `crates/sandbox-runtime/operation/src/layerstack/service/impls/mod.rs`
  - Update the operation entry constant for the new dispatch function pointer.
- `crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs`
  - Add optional trace parameter to dispatch only.
  - Wrap only `operations.layerstack.squash()` in `LayerStackService::squash`.

Do not change public service method signatures. Do not add trace parameters to
`CommandOperationService` or `LayerStackService` methods.

Daemon files:

- `crates/sandbox-daemon/src/server/dispatch.rs`
  - Create `Some(OperationTrace::new())` only when daemon observability is
    enabled and `sandbox_id` is present.
  - Pass `trace.as_ref()` into runtime dispatch.
  - Complete the trace after projecting the response JSON.
  - Persist completed trace before Phase 2 snapshot collection.
- `crates/sandbox-daemon/src/observability/service.rs`
  - Add daemon-owned trace persistence entrypoint.
  - Map `CompletedOperationTrace` and `CompletedOperationSpan` into existing
    `TraceRecord` and `SpanRecord`.
  - Bound error strings consistently with the current daemon service helpers.
  - Keep snapshot collection behavior unchanged.

Storage files:

- No production storage changes are required.
- Add a test-only trace read helper only if daemon trace persistence tests cannot
  assert through existing store helpers and direct SQLite reads.

Do not add a new crate. Do not add manager files.

## Expected Struct and Signature Changes

Expected runtime trace DTOs:

```rust
pub struct CompletedOperationTrace {
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub duration_ms: f64,
    pub spans: Vec<CompletedOperationSpan>,
}

pub struct CompletedOperationSpan {
    pub parent_call_index: Option<i64>,
    pub method_name: &'static str,
    pub call_index: i64,
    pub status: &'static str,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub duration_ms: f64,
}
```

Runtime completed DTOs intentionally omit trace ids, storage span ids,
`sandbox_id`, `request_id`, operation, workspace hierarchy, command hierarchy,
and response error metadata. Runtime span names and runtime span statuses stay as
`&'static str`; the daemon converts them to storage `String`s during mapping.

Expected dispatch signatures:

```rust
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response;

pub(crate) fn dispatch(
    operations: &SandboxRuntimeOperations,
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response;
```

Public service signatures remain unchanged.

Expected daemon persistence signature:

```rust
impl DaemonObservability {
    pub(crate) fn insert_completed_operation_trace(
        &self,
        sandbox_id: String,
        request_id: String,
        operation: String,
        response: &serde_json::Value,
        trace: CompletedOperationTrace,
    ) -> Result<(), StoreError>;
}
```

The server helper should skip work if `self.observability` is absent before
calling this method. `DaemonObservability` maps runtime call indexes to storage
span ids and derives trace status/error fields from `response`.

## Failure Policy

Observability remains best effort.

- Missing `sandbox_id` disables trace persistence and must not fail daemon
  serving.
- Failure to open `DaemonObservability` disables trace persistence and must not
  fail daemon serving.
- `ObservabilityStore::insert_trace` failures must not change operation
  responses.
- Trace persistence failures must not prevent Phase 2 snapshot collection.
- Phase 2 snapshot collection failures must not change operation responses.
- Runtime operation errors still return the same operation response shape and
  are recorded only as trace/span status and bounded error metadata.
- Unknown operations still return `Response::unknown_op()` and record a request
  trace when observability is enabled.
- Request argument parse errors still return the current `invalid_request`
  response shape and record the trace status when observability is enabled.
- Command output, transcript content, stdout/stderr chunks, and shell input text
  must not be copied into trace or span records.

## LOC Budget

`crates/sandbox-runtime` non-test LOC target: `70-120`.

Implementation should aim for `75-95` non-test LOC. Treat anything above `120`
as evidence that Phase 3.5 spans, disabled trace plumbing, storage-shaped DTOs,
or module ceremony slipped back in.

Expected split:

```text
OperationTrace + span guard/types            45-65
dispatch boundary plumbing                   15-20
selected operation wrapper spans             10-15
exports/module movement, if needed            0-5
```

If runtime non-test LOC trends above 120, stop and narrow the boundary. The usual
cause is accidentally implementing Phase 3.5 lower-level spans, disabled/no-op
trace state, storage DTO fields, public service signature churn, or Phase 4/4.5
trace linking in Phase 3.

Daemon LOC is outside this runtime budget, but should stay focused: one daemon
trace mapper, no production storage migration, and focused tests.

## Verification Plan

Run required checks:

```sh
cargo fmt --check
cargo check -p sandbox-observability --tests
cargo test -p sandbox-observability
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime operation_trace
cargo check -p sandbox-daemon --tests
cargo test -p sandbox-daemon observability
```

Required behavior tests:

- schema migration count remains unchanged for Phase 3 unless another completed
  phase has already added migrations;
- `traces.workspace_id`, `traces.command_session_id`,
  `idx_traces_workspace_time`, and `idx_traces_command_time` are not introduced
  by this phase;
- synthetic request trace persists all spans under one `trace_id`;
- span `call_index` ordering is stable;
- nested runtime spans map to the correct storage `parent_span_id`;
- early returns close active spans;
- operation errors still persist trace status without changing response shape;
- unknown operations persist a request/root trace when observability is enabled;
- operation argument parse errors persist a request/root trace when
  observability is enabled;
- missing `sandbox_id` disables persistence without failing requests;
- observability store failures do not change operation responses;
- runtime tests do not import `sandbox-observability`;
- selected operation traces contain only root, operation dispatch, and one public
  service-method span;
- public service method signatures do not change;
- command output and transcript content do not appear in trace or span records.

Suggested focused test placement:

- `crates/sandbox-runtime/operation/tests/operation_trace.rs`
  - span nesting;
  - call index order;
  - early return guard closure;
  - panic-unwind guard closure status;
  - selected span set with fake services where practical;
  - no `sandbox-observability` import.
- `crates/sandbox-observability/tests/schema.rs`
  - existing synthetic trace insertion still works;
  - no Phase 3 hierarchy columns or indexes are added.
- `crates/sandbox-daemon/tests/unit/observability.rs`
  - completed trace mapping and persistence;
  - missing sandbox id;
  - store failure does not alter response;
  - unknown operation trace persistence.

## Completion Criteria

Storage:

- [x] `observability.sqlite` remains the only observability database.
- [x] No Phase 3 schema migration is added.
- [x] `traces.workspace_id` is not added in Phase 3.
- [x] `traces.command_session_id` is not added in Phase 3.
- [x] `idx_traces_workspace_time` is not added in Phase 3.
- [x] `idx_traces_command_time` is not added in Phase 3.
- [x] No `trace_links` table is created.
- [x] No async trace columns are added in Phase 3.

Runtime boundary:

- [x] `OperationTrace` lives under `crates/sandbox-runtime/operation`.
- [x] Runtime does not depend on `sandbox-observability`.
- [x] Runtime does not import `rusqlite`.
- [x] Runtime does not know daemon paths or `ObservabilityStore`.
- [x] Runtime DTOs do not store `trace_id`, `sandbox_id`, `request_id`,
  operation, workspace hierarchy, command hierarchy, or response error metadata.
- [x] Runtime public dispatch accepts `Option<&OperationTrace>`.
- [x] Operation entries and selected dispatch functions accept
  `Option<&OperationTrace>`.
- [x] Public service method signatures are unchanged.
- [x] The root `dispatch_operation` span is automatic.
- [x] The selected operations contain only root, operation dispatch, and one
  public service-method span.
- [x] Runtime non-test LOC stays within `70-120`, with `75-95` preferred.

Daemon boundary:

- [x] Daemon creates enabled traces only when observability is enabled and
  `sandbox_id` is available.
- [x] Daemon passes `None` when persistence is disabled.
- [x] Daemon persists completed traces after response projection.
- [x] Daemon derives trace status and bounded error metadata from projected
  response JSON exactly once.
- [x] Daemon does not change operation response payloads.
- [x] Daemon-owned code maps runtime trace DTOs into `TraceRecord` and
  `SpanRecord`.
- [x] Trace persistence failures do not change user operation responses.
- [x] Phase 2 snapshot collection still runs after request handling.

Data boundary:

- [x] `sandbox_protocol::Response` remains a raw payload wrapper.
- [x] No `{ "result": ..., "meta": ... }` response envelope is introduced.
- [x] Command output is not written to trace or span storage.
- [x] Transcript content is not written to trace or span storage.
- [x] External observability services are not required.

Verification:

- [x] `cargo fmt --check` passes.
- [x] `cargo check -p sandbox-observability --tests` passes.
- [x] `cargo test -p sandbox-observability` passes.
- [x] `cargo check -p sandbox-runtime --tests` passes.
- [x] `cargo test -p sandbox-runtime operation_trace` passes.
- [x] `cargo check -p sandbox-daemon --tests` passes.
- [x] `cargo test -p sandbox-daemon observability` passes.

## Open Questions

- The Phase 2 implementation is present in live code, but
  `docs/observability/phase-2-runtime-snapshots.md` still has an unchecked
  completion checklist. That doc cleanup is separate from Phase 3 and should not
  block this spec.
- Panic trace persistence is deferred. Normal operation errors, unknown
  operations, and argument parse errors are required Phase 3 persistence cases.
