# Phase 3.5 Targeted Deep Request Spans

Status: draft implementation spec

## Parent spec

[sandbox-observability.md](./sandbox-observability.md)

## Builds on

- [phase-1-observability-foundation.md](./phase-1-observability-foundation.md)
- [phase-2-runtime-snapshots.md](./phase-2-runtime-snapshots.md)
- [phase-3-request-method-traces.md](./phase-3-request-method-traces.md)

## Exact Goal

Phase 3.5 adds generic, automatic, and dynamic targeted child spans under
selected Phase 3 request-method spans. It does not add a profiler. It adds one
stable span-key namespace, one request-local enabled-key set, and one child span
measurement API so proven-slow or ambiguous parent spans can be split on future
requests without changing operation response shape or storage schema.

The exact deliverable is:

- keep Phase 3 root, operation-dispatch, and public service-method spans;
- add stable `SpanKey` values for eligible deep in-process spans;
- let `OperationTrace` directly carry enabled `SpanKey` values for the request;
- add a `measure_if`-style API that records a child span only when the request
  enabled set contains the key;
- have daemon dispatch pass a snapshot of daemon-local enabled keys into
  `OperationTrace` before calling `sandbox_runtime::dispatch_operation`;
- update that daemon-local enabled-key set after completed request traces show
  slow or ambiguous Phase 3 parent spans;
- persist enabled child spans through the existing `traces` and `spans` rows.

Phase 3.5 must preserve these boundaries:

- no changes to `sandbox_protocol::Response`;
- no `{ result, meta }` response envelopes;
- no command transcript or command output ingestion into observability;
- no `sandbox-observability`, `rusqlite`, daemon path, or store dependency from
  `crates/sandbox-runtime`;
- no new SQLite tables, indexes, migrations, query APIs, `trace_links`, manager
  aggregation, background tuning workers, profiler integration, or broad
  `tracing` attributes.

Generic means future operations can reuse the same `SpanKey` plus `measure_if`
mechanism. Automatic means each call site only names an exported key constant and
closure; the API checks the request-local enabled set and disabled keys run the
original code path. Dynamic means the daemon-local enabled set can change between
requests based on recently completed local traces.

## Current Repo Grounding

This section describes the live checkout. Docs are proposals; live code is the
source of truth.

### Phase 3 Runtime Trace Types

`crates/sandbox-runtime/operation/src/observability.rs` already defines:

- `OperationTrace`;
- `SpanGuard`;
- `CompletedOperationTrace`;
- `CompletedOperationSpan`.

The current `OperationTrace` shape is:

```rust
pub struct OperationTrace {
    state: RefCell<TraceState>,
}
```

`TraceState` currently stores:

- `started_at: Instant`;
- `started_at_unix_ms: i64`;
- `active_stack: Vec<i64>`;
- `completed: Vec<CompletedOperationSpan>`;
- `next_call_index: i64`.

`CompletedOperationSpan` currently stores only runtime-local span facts:

- `parent_call_index`;
- `method_name`;
- `call_index`;
- `status`;
- start/finish Unix timestamps;
- `duration_ms`.

This is a good Phase 3.5 boundary. `OperationTrace` can accept an enabled
span-key set without storing SQLite handles, daemon paths, request ids, sandbox
ids, storage row ids, response JSON, command output, or
`sandbox-observability` records.

### Current Runtime Span Helpers

`crates/sandbox-runtime/operation/src/observability.rs` currently has:

```rust
pub(crate) fn measure_optional<T>(
    trace: Option<&OperationTrace>,
    method_name: &'static str,
    call: impl FnOnce() -> T,
) -> T
```

It records a Phase 3 span when `trace` is `Some` and calls through directly when
`trace` is `None`. Phase 3.5 should leave this helper as the coarse-span helper
and add child-key gating beside it, not replace every Phase 3 call site.

### Current Runtime Dispatch Signatures

`crates/sandbox-runtime/operation/src/operation.rs` currently defines:

```rust
type OperationDispatch = fn(
    &SandboxRuntimeOperations,
    &sandbox_protocol::Request,
    Option<&OperationTrace>,
) -> sandbox_protocol::Response;
```

`OperationEntry` stores that dispatch function pointer, and
`dispatch_operation` currently has this signature:

```rust
pub(crate) fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response
```

`crates/sandbox-runtime/operation/src/lib.rs` re-exports:

```rust
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response
```

`dispatch_operation` records the Phase 3 root span named
`dispatch_operation`, then records an operation dispatch span such as
`exec_command::dispatch` or `squash::dispatch` before calling the selected
operation dispatch function.

### Current Phase 3 Service-Method Spans

The selected Phase 3 service-method spans are currently recorded in operation
dispatch wrappers:

- `crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs`
  wraps `operations.command.exec_command(input)` in
  `CommandOperationService::exec_command`;
- `crates/sandbox-runtime/operation/src/command/service/impls/write_command_stdin.rs`
  wraps `operations.command.write_command_stdin(input)` in
  `CommandOperationService::write_command_stdin`;
- `crates/sandbox-runtime/operation/src/command/service/impls/read_command_lines.rs`
  wraps `operations.command.read_command_lines(input)` in
  `CommandOperationService::read_command_lines`;
- `crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs`
  wraps `operations.layerstack.squash()` in `LayerStackService::squash`.

Phase 3.5 child spans should nest below those coarse parent spans. They should
not replace the parent spans.

### Current Daemon Dispatch and Trace Completion

`crates/sandbox-daemon/src/server/dispatch.rs` currently:

- decodes bytes into `sandbox_protocol::Request`;
- validates daemon scope;
- clones `self.observability`;
- creates `Some(OperationTrace::new())` only when observability exists and
  `sandbox_id` is present and non-empty;
- calls `sandbox_runtime::dispatch_operation(&operations, &request,
  trace.as_ref())` inside `tokio::task::spawn_blocking`;
- projects the runtime response with `into_json_value`;
- completes the trace with `OperationTrace::complete`;
- calls `DaemonObservability::insert_completed_operation_trace`;
- ignores trace persistence failures for the user response.

Phase 3.5 should keep this ownership. Daemon dispatch decides whether tracing is
enabled and passes request-local enabled child span keys before the blocking
runtime call. Runtime receives only `Option<&OperationTrace>`.

### Current Daemon Trace Mapping

`crates/sandbox-daemon/src/observability/service.rs` currently implements
`DaemonObservability::insert_completed_operation_trace`. It:

- derives `trace_id` as `request:<request_id>`;
- derives request status and bounded error metadata from the projected response
  JSON;
- maps `CompletedOperationTrace` into one `TraceRecord`;
- maps each `CompletedOperationSpan` into one `SpanRecord`;
- derives storage `span_id` from trace id plus runtime `call_index`;
- derives `parent_span_id` from runtime `parent_call_index`;
- marks the deepest completed span as `error` when the projected response is a
  fault;
- calls `ObservabilityStore::insert_trace`.

This mapping already persists arbitrary additional completed spans. Phase 3.5
does not need storage or schema mapping changes, but it must update
response-error attribution so child spans are not marked as failed merely because
they have the largest `call_index`.

### Current Store Shape

`crates/sandbox-observability/src/records.rs` currently defines `TraceRecord`
and `SpanRecord` with enough fields for Phase 3.5:

- trace identity, kind, status, sandbox id, operation, optional request id,
  timing, and bounded error metadata;
- span identity, trace id, optional parent span id, `method_name`,
  `call_index`, status, timing, and bounded error metadata.

`crates/sandbox-observability/src/store.rs` already creates `traces` and
`spans` in the Phase 1 migration and exposes:

```rust
pub fn insert_trace(
    &self,
    trace: &TraceRecord,
    spans: &[SpanRecord],
) -> Result<(), StoreError>
```

The schema already has `idx_spans_trace_call_index`. It does not need
`trace_links`, hierarchy columns, response metadata columns, or new indexes for
Phase 3.5.

### Candidate Child-Span Call Sites

Current in-process call sites:

- `CommandOperationService::exec_validated_command` in
  `crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs`
  calls `resolve_exec_workspace`, `start_command_process`, and
  `initial_exec_yield`.
- `CommandOperationService::resolve_exec_workspace` in the same file calls
  `WorkspaceSessionService::resolve_session` for existing sessions and
  `WorkspaceSessionService::create_workspace_session` for one-shot sessions.
- `CommandOperationService::start_command_process` in the same file calls
  `ResolvedExecWorkspace::entry` and then `CommandLaunchDriver::spawn`.
- `RealCommandLaunchDriver::spawn` in
  `crates/sandbox-runtime/operation/src/command/service/launch.rs` calls
  `CommandProcessSpawn::prepare` and `CommandProcess::spawn`.
- `LayerStackService::squash` in
  `crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs`
  calls `sandbox_runtime_layerstack::LayerStack::open` and then
  `LayerStack::squash`.

Current lower-crate or process-boundary call sites:

- `WorkspaceRuntimeService::create_workspace` in
  `crates/sandbox-runtime/workspace/src/service/impls/create_workspace.rs`
  calls `sandbox_runtime_layerstack::service::acquire_snapshot_with_lease` and
  workspace mode setup. This is below the operation crate.
- `CommandProcess::spawn` in
  `crates/sandbox-runtime/command/src/process.rs` builds a
  `NamespaceRunnerRequest`, calls `spawn_current_exe_ns_runner`, and waits for
  the start acknowledgement.
- `spawn_current_exe_ns_runner` in
  `crates/sandbox-runtime/command/src/pty.rs` spawns the current executable as
  `ns-runner`.
- `runner::run`, `run_setns`, and `shell_exec::execute_shell` live under
  `crates/sandbox-runtime/namespace-process/src/runner`. They run in the
  namespace-runner process path and are Phase 4.5, not ordinary Phase 3.5 child
  spans.

Phase 3.5 may wrap lower-crate calls from the caller side, but it must not make
lower workspace, layerstack, command, or namespace-process crates depend on
`sandbox-runtime/operation`, `OperationTrace`, or `sandbox-observability`.

## Non-Goals

Phase 3.5 does not implement:

- automatic discovery of every Rust function call;
- broad `tracing` attribute adoption;
- compiler instrumentation;
- eBPF or profiler integration;
- new observability tables, columns, indexes, or migrations;
- background tuning workers;
- percentile math;
- daemon or manager query APIs;
- `trace_links`;
- Phase 4 async finalization traces;
- Phase 4.5 cross-process namespace-runner traces;
- manager aggregation;
- public response-shape changes;
- response envelopes such as `{ result, meta }`;
- command transcript or command output ingestion;
- runtime imports from `sandbox-observability`, `rusqlite`, daemon paths, store
  types, or record types.

## Architecture

### Generic Span Keys

Add a small runtime-side value model in
`crates/sandbox-runtime/operation/src/observability.rs`.

Recommended shape:

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SpanKey(&'static str);
```

`SpanKey` is a stable domain key, not a storage id and not a Rust function name.
The first implementation should use the same string for the span key and the
persisted `SpanRecord.method_name`. There is no current need for a separate
display label. If a later product UI wants friendly names, it can map stable
keys at query/render time without changing trace rows.

Because the tuple field is private, the runtime crate must export the selected
key constants or a small `span_keys` namespace that contains only constants.
Daemon code should construct enabled sets from those exported constants, not from
free-form strings. Do not make the tuple field public and do not add a broad
string parser before a real config surface exists.

`OperationTrace` directly stores the request's enabled `SpanKey` values. This
set must not store:

- SQLite handles;
- daemon paths;
- request ids;
- sandbox ids;
- response JSON;
- storage row ids;
- command output;
- transcript paths or transcript rows;
- `sandbox-observability` records.

Add `enabled_span_keys: HashSet<SpanKey>` to `OperationTrace`, preferably
outside the `RefCell<TraceState>` because it is immutable for one request. Keep
`OperationTrace::new()` as a default empty-set constructor for tests and add one
explicit constructor:

```rust
impl OperationTrace {
    pub fn new() -> Self;
    pub fn new_with_enabled_span_keys(keys: impl IntoIterator<Item = SpanKey>) -> Self;
}
```

The empty enabled set records Phase 3 spans but records no Phase 3.5 child
spans.

### Enabled-Key Construction

Daemon dispatch snapshots the current enabled-key set. Runtime does not read
daemon config, SQLite, observability stores, or local paths.

Add daemon-local in-memory state to `DaemonObservability`:

```rust
enabled_deep_span_keys: Mutex<HashSet<SpanKey>>
```

Add a method such as:

```rust
pub(crate) fn enabled_deep_span_keys(&self) -> Vec<SpanKey>
```

`SandboxDaemonServer::dispatch_request` changes trace construction from
`OperationTrace::new()` to:

```rust
let trace = observability
    .as_ref()
    .zip(trace_sandbox_id.as_ref())
    .map(|(observability, _)| {
        OperationTrace::new_with_enabled_span_keys(observability.enabled_deep_span_keys())
    });
```

The first implementation should use recent completed local traces as the
dynamic input. After a completed trace is available, daemon code observes
Phase 3 parent spans and enables a group of child keys for future requests when
a parent span crosses a simple constant threshold.

Recommended first threshold:

```rust
const DEEP_SPAN_PARENT_THRESHOLD_MS: f64 = 100.0;
```

This threshold is intentionally simple. It is not a percentile, not a moving
average, and not a stored tuning rule. It can be adjusted after real local trace
data exists.

Suggested parent-to-key groups:

- slow `CommandOperationService::exec_command` enables command exec child keys;
- slow `LayerStackService::squash` enables layerstack squash child keys.

The enabled set may be monotonic for the daemon process lifetime. That is enough
for the first dynamic implementation and avoids a cache, decay worker, timer,
query API, or config surface. If config-driven enablement is added in a later
phase, it should union validated exported constants with the same in-memory
enabled set before constructing `OperationTrace`; runtime call sites do not
change.

### Span Key Registry

Keep the initial key registry close to the runtime trace code, either as
constants in `crates/sandbox-runtime/operation/src/observability.rs` or as a
nested `span_keys` module in that same file. Do not add a new crate.

Initial registered keys:

| Span key | First Phase 3.5 decision |
| --- | --- |
| `command.exec.workspace.resolve` | Register and wire in `CommandOperationService::exec_validated_command` around the workspace-resolution branch. |
| `command.exec.workspace.resolve_existing_session` | Register and wire in the existing-session branch around `WorkspaceSessionService::resolve_session`. |
| `command.exec.workspace.create_one_shot_session` | Register and wire in the one-shot branch around `WorkspaceSessionService::create_workspace_session`. |
| `command.exec.process.start` | Register and wire in `exec_validated_command` around the process-start boundary. |
| `layerstack.squash.open_stack` | Register and wire in `LayerStackService::squash` around opening the layer stack. |
| `layerstack.squash.compact_stack` | Register and wire in `LayerStackService::squash` around the squash operation. |

Candidate keys to defer from the first implementation:

| Span key | Decision |
| --- | --- |
| `command.exec.workspace_runtime.create_workspace` | Caller-owned boundary span is possible in `WorkspaceSessionService::create_workspace_session`, but defer until `command.exec.workspace.create_one_shot_session` proves too broad. Do not pass trace context into `crates/sandbox-runtime/workspace`. |
| `command.exec.layerstack.snapshot_or_lease` | Defer. The live call is inside `WorkspaceRuntimeService::create_workspace` through `sandbox_runtime_layerstack::service::acquire_snapshot_with_lease`; splitting it would push trace context below the operation crate. |
| `command.exec.workspace.entry_materialize` | Defer. First measure the broader process-start boundary; split workspace entry materialization only if the parent remains ambiguous. |
| `command.exec.spawn.prepare_artifacts` | Defer. It requires `CommandLaunchDriver::spawn` signature churn and fake-driver updates; first measure the broader process-start boundary. |
| `command.exec.spawn.runtime_process` | Defer. It requires `CommandLaunchDriver::spawn` signature churn and reaches toward lower command-crate work; first measure the broader process-start boundary. |
| `command.exec.spawn.build_namespace_runner_request` | Defer. The live function is in `crates/sandbox-runtime/command/src/process.rs`; the first-pass `command.exec.process.start` span covers it inclusively. |
| `command.exec.spawn.spawn_current_exe_ns_runner` | Defer. The live function is in `crates/sandbox-runtime/command/src/pty.rs`; the first-pass `command.exec.process.start` span covers it inclusively. |

Do not register deferred keys until they have real call sites. Future config or
operator-supplied strings are out of scope for the first implementation; when
they exist later, unknown strings should be ignored until they map to an exported
constant.

### Runtime API

Add one public child-span API on `OperationTrace`:

```rust
impl OperationTrace {
    pub fn measure_if<T>(
        &self,
        span_key: SpanKey,
        call: impl FnOnce() -> T,
    ) -> T {
        if self.is_span_key_enabled(span_key) {
            self.measure(span_key.as_str(), call)
        } else {
            call()
        }
    }
}
```

The API records the child span only when the request enabled set contains the key.
Disabled keys must execute the closure directly. They must not allocate a span,
reserve a call index, append a skipped span row, or alter parentage/order of
enabled spans.

To keep call sites readable, a crate-private optional helper is acceptable:

```rust
pub(crate) fn measure_optional_if<T>(
    trace: Option<&OperationTrace>,
    span_key: SpanKey,
    call: impl FnOnce() -> T,
) -> T
```

This helper should simply call `trace.measure_if(span_key, call)` when a trace
exists and call through directly when tracing is disabled. It is not a second
configuration model.

### Daemon Enabled-Key State

`DaemonObservability` owns the daemon-local enabled set. It should expose only
two narrow methods:

```rust
pub(crate) fn enabled_deep_span_keys(&self) -> Vec<SpanKey>;

fn update_enabled_deep_span_keys(
    &self,
    trace: &CompletedOperationTrace,
);
```

The update method should inspect only completed runtime span names and
durations. It should not inspect command output, transcript data, response
payload content, SQLite state, or query results.

Enabled-key update runs in the same best-effort observability path as trace
persistence. Lock poisoning should not fail user operations; follow the repo's
existing pattern of recovering the inner value or skipping the enabled-key
update.

### Storage and Response Shape

Phase 3.5 stores child spans as ordinary `SpanRecord` rows through the existing
Phase 3 persistence path:

```text
OperationTrace
  CompletedOperationTrace
    CompletedOperationSpan[]
      DaemonObservability::insert_completed_operation_trace
        TraceRecord
        SpanRecord[]
          ObservabilityStore::insert_trace
```

No storage schema change is required. No response metadata is added. The
runtime response is projected exactly as it is today before trace insertion.

Child span keys are stored in `SpanRecord.method_name`. Parentage uses the
existing `call_index` and `parent_call_index` mapping. Existing tests that sort
spans by `call_index` remain the right model.

Phase 3.5 must also adjust daemon error attribution to stop assuming that the
deepest completed span is the response-error span. That Phase 3 shortcut worked
while each operation had only root, dispatch, and one public service-method span,
but child spans may finish successfully before later parent code returns a fault.
Response-derived operation errors should remain attached to the narrowest
applicable Phase 3 coarse span, usually the public service-method span, unless a
later design adds explicit runtime span-error reporting. Child spans must not get
response-derived error metadata merely because they have the largest
`call_index`. This is a small daemon mapping/test change, not a storage schema
change.

### Crate and Process Boundaries

Phase 3.5 spans stay on the same request trace only when the work runs in the
daemon or runtime process and can be measured from the operation crate.

Boundary rules:

- do not add a dependency from `crates/sandbox-runtime/workspace`,
  `crates/sandbox-runtime/layerstack`, `crates/sandbox-runtime/command`, or
  `crates/sandbox-runtime/namespace-process` back to
  `crates/sandbox-runtime/operation`;
- do not add `sandbox-observability` types to lower runtime crates;
- when a lower crate cannot accept neutral trace context cleanly, keep a
  caller-owned boundary span around the lower call;
- parent-side launch-driver spans are eligible for a later split because
  `RealCommandLaunchDriver` lives in `crates/sandbox-runtime/operation`, but they
  are deferred from the first implementation to avoid launch-driver trait churn;
- `runner::run`, `run_setns`, namespace setup, shell execution, and command
  wait-loop internals are Phase 4.5 and must not appear as Phase 3.5 child
  spans.

The extension point for lower crates is to expose smaller ordinary functions or
return richer typed timing data later. Phase 3.5 should not create a generic
lower-crate tracing trait just to reach those internals.

## Self-Critical Architecture Check

### Simplicity

The smallest design that satisfies generic, automatic, and dynamic child span
enablement is:

```text
SpanKey
OperationTrace { enabled span keys }
OperationTrace::measure_if
DaemonObservability in-memory enabled-key set
```

Yes, the design reduces to an enabled-key set plus `measure_if`. It does not
need a new table, background worker, query API, registry module file, trait
hierarchy, persistent cache, or public config surface for the first
implementation.

If the first implementation had to fit in the lower half of the 60-130 runtime
LOC budget, delete these pieces first:

- config-driven enablement;
- TTL or decay of enabled keys;
- separate display names for span keys;
- launch-driver/internal spawn keys;
- deferred lower-crate keys;
- `command.exec.workspace_runtime.create_workspace`;
- any helper beyond `measure_optional_if`.

The happy path remains readable when call sites look like:

```rust
measure_optional_if(trace, span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE, || {
    self.resolve_exec_workspace(&input, trace)
})?
```

If call sites require local registries, trait objects, span builders, or manual
start/finish calls, the design is too complex for Phase 3.5 and should be
reduced.

### Genericity

The mechanism is generic because `SpanKey`, `OperationTrace::measure_if`, and
the request-local enabled set are reusable. It is not generic because every
Rust function is automatically instrumented.

A future operation adds child spans by:

- adding stable `SpanKey` constants;
- adding those keys to the daemon parent-to-key enablement mapping;
- wrapping selected in-process boundaries with `measure_optional_if`.

It should not need operation-specific daemon persistence paths.

Span keys are stable domain names such as
`command.exec.workspace.create_one_shot_session`. They are not storage ids,
display labels, or exact function names such as `resolve_exec_workspace`, which
may churn during refactors.

The enabled-key set remains independent of SQLite, daemon paths, request ids,
response JSON, command output, transcripts, and `sandbox-observability` records.

### Extensibility

The next runtime operation can add eligible child spans with the same three-step
pattern: define keys, add an enablement mapping from a Phase 3 parent span, and
wrap in-process boundaries with `measure_optional_if`.

If a later phase adds config-driven enablement, it can compose with
recent-trace-driven enablement by unioning validated exported constants in
`DaemonObservability::enabled_deep_span_keys`. Runtime call sites do not change.

If later phases add a product query API, no runtime change is required. The API
can query existing `traces` and `spans` rows and map stable span-key strings to
display labels outside runtime.

Phase 4 remains responsible for async finalization traces and trace links.
Phase 4.5 remains responsible for namespace-runner process traces and runner
internals. Manager aggregation remains out of scope.

The lower-crate extension point is caller-owned boundary spans. Phase 3.5 does
not create a dependency from lower runtime crates back to the operation crate.

### Rejection Criteria

Reject or revise the Phase 3.5 design if implementation requires any of:

- a new persistent schema;
- a profiler-like function discovery system;
- broad `tracing` annotations;
- a background tuning service;
- a large trait hierarchy for span enablement;
- operation-specific daemon persistence paths;
- cross-process runner internals as ordinary child spans;
- public response-shape changes.

The self-critical check finds that the simpler enabled-key set plus
`measure_if` design works. Phase 3.5 should use that design.

## Detailed File Plan

Keep the implementation in the existing crate layout.

### Runtime Operation Trace

`crates/sandbox-runtime/operation/src/observability.rs`

- Add `SpanKey`.
- Add initial span-key constants, preferably in a nested `span_keys` namespace.
- Add `enabled_span_keys: HashSet<SpanKey>` to `OperationTrace`.
- Keep `OperationTrace::new()` as an empty-enabled-set constructor.
- Add `OperationTrace::new_with_enabled_span_keys(keys)`.
- Add `OperationTrace::measure_if`.
- Add crate-private `measure_optional_if` only if it keeps call sites readable.
- Do not import `sandbox-observability`, SQLite, daemon config, request ids,
  response JSON, or command output.

`crates/sandbox-runtime/operation/src/lib.rs`

- Re-export `SpanKey` for daemon enabled-key construction.
- Re-export the selected span-key constants or a narrow `span_keys` namespace so
  daemon code never needs to manufacture keys from arbitrary strings.
- Continue re-exporting `OperationTrace`, `CompletedOperationTrace`, and
  `CompletedOperationSpan`.

`crates/sandbox-runtime/operation/src/operation.rs`

- No signature change is required for `dispatch_operation`.
- Keep Phase 3 `dispatch_operation` and `<operation>::dispatch` spans.
- Do not gate Phase 3 coarse spans with Phase 3.5 enabled keys.

### Command Runtime Spans

`crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs`

- Pass `trace: Option<&OperationTrace>` from dispatch into
  `CommandOperationService::exec_command`.
- Thread `trace` through `exec_validated_command`, `resolve_exec_workspace`,
  and `start_command_process`.
- Wrap:
  - `resolve_exec_workspace` with `command.exec.workspace.resolve`;
  - `WorkspaceSessionService::resolve_session` with
    `command.exec.workspace.resolve_existing_session`;
  - `WorkspaceSessionService::create_workspace_session` with
    `command.exec.workspace.create_one_shot_session`;
  - `start_command_process` with `command.exec.process.start`.
- Do not add command output, transcript text, environment data, or shell text to
  spans.

`crates/sandbox-runtime/operation/src/command/service/launch.rs`

- No first-pass signature change is required.
- Leave `CommandLaunchDriver::spawn`, fake launch drivers, and
  `RealCommandLaunchDriver::spawn` unchanged unless `command.exec.process.start`
  later proves too broad.
- Do not pass trace context into `crates/sandbox-runtime/command`.

`crates/sandbox-runtime/operation/src/command/service/helpers.rs`

- No first-pass child spans are required. Leave `wait_for_command_yield` and
  transcript reads under the existing Phase 3 parent span unless future Phase 3
  data proves this path ambiguous.

`crates/sandbox-runtime/operation/src/command/service/core.rs`

- No state-field change should be required.
- No launch-driver signature change should be required in the first
  implementation.

`crates/sandbox-runtime/operation/src/command/service/impls/write_command_stdin.rs`

- No Phase 3.5 child spans in the first implementation.
- Keep the Phase 3 `CommandOperationService::write_command_stdin` span.

`crates/sandbox-runtime/operation/src/command/service/impls/read_command_lines.rs`

- No Phase 3.5 child spans in the first implementation.
- Keep the Phase 3 `CommandOperationService::read_command_lines` span.

### Layerstack Runtime Spans

`crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs`

- Pass `trace: Option<&OperationTrace>` from dispatch into
  `LayerStackService::squash`.
- Wrap `LayerStack::open` with `layerstack.squash.open_stack`.
- Wrap `stack.squash()` with `layerstack.squash.compact_stack`.
- Keep result mapping and `SquashLayerStackResult` unchanged.

`crates/sandbox-runtime/layerstack/src/stack/ops/squash.rs`

- No Phase 3.5 changes. The operation crate owns caller-side spans around
  `LayerStack::squash`.

### Workspace Runtime Boundary

`crates/sandbox-runtime/operation/src/workspace_session/service/impls/create_workspace_session.rs`

- No first-pass signature change is required if the
  `command.exec.workspace.create_one_shot_session` span is enough.
- Defer `command.exec.workspace_runtime.create_workspace` until traces show the
  create-one-shot-session span is still ambiguous.

`crates/sandbox-runtime/operation/src/workspace_session/service/impls/resolve_session.rs`

- No first-pass signature change is required; the command call site can wrap
  `resolve_session`.

`crates/sandbox-runtime/workspace/src/service/impls/create_workspace.rs`

- No Phase 3.5 changes. Do not add `OperationTrace` or
  `sandbox-observability` to the workspace crate.

### Command Lower-Crate Boundary

`crates/sandbox-runtime/command/src/process.rs`

- No Phase 3.5 changes in the first implementation.
- `CommandProcess::spawn` remains covered inclusively by the caller-owned
  `command.exec.process.start` span in the operation crate.

`crates/sandbox-runtime/command/src/pty.rs`

- No Phase 3.5 changes.
- `spawn_current_exe_ns_runner` remains inside the lower command crate and is
  covered inclusively by `command.exec.process.start`.

### Daemon Enabled Keys

`crates/sandbox-daemon/src/server/dispatch.rs`

- Construct `OperationTrace` with
  `OperationTrace::new_with_enabled_span_keys` when observability is enabled.
- Keep `None` when observability is disabled.
- Do not create a disabled/no-op trace object.
- Do not change response projection or error response shape.

`crates/sandbox-daemon/src/observability/service.rs`

- Add daemon-local `enabled_deep_span_keys: Mutex<HashSet<SpanKey>>`.
- Add `enabled_deep_span_keys`.
- Update enabled-key state from `CompletedOperationTrace` before consuming it into
  storage rows.
- Update response-error attribution so completed child spans are not marked
  failed solely because they have the largest `call_index`.
- Keep `insert_completed_operation_trace` best-effort and request-safe.
- Keep trace mapping to existing `TraceRecord` and `SpanRecord`.

`crates/sandbox-daemon/tests/unit/observability.rs`

- Add daemon enabled-key tests:
  - missing `sandbox_id` still disables traces;
  - store failures still do not change operation responses;
  - completed slow `CommandOperationService::exec_command` enables command
    child keys for future requests;
  - completed slow `LayerStackService::squash` enables layerstack child keys;
  - response-derived operation errors mark the appropriate Phase 3 coarse span,
    not the deepest child span by default;
  - successful trace insertion still writes ordinary `SpanRecord` rows.

### Runtime Tests

`crates/sandbox-runtime/operation/tests/operation_trace.rs`

- Add child span tests:
  - default enabled set disables child spans;
  - enabled key records a child span under the current parent;
  - disabled keys do not consume `call_index`;
  - mixed enabled and disabled keys keep stable call ordering;
  - existing Phase 3 coarse spans still appear.

Runtime tests must not import `sandbox-observability`.

## Expected Struct and Signature Changes

Expected runtime additions:

```rust
pub struct SpanKey(&'static str);

pub mod span_keys {
    pub const COMMAND_EXEC_WORKSPACE_RESOLVE: SpanKey = SpanKey("command.exec.workspace.resolve");
    // additional first-pass constants live here
}

impl OperationTrace {
    pub fn new() -> Self;
    pub fn new_with_enabled_span_keys(keys: impl IntoIterator<Item = SpanKey>) -> Self;
    pub fn measure_if<T>(&self, span_key: SpanKey, call: impl FnOnce() -> T) -> T;
}
```

Expected command service signature changes:

```rust
impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
        trace: Option<&OperationTrace>,
    ) -> Result<CommandYield, CommandServiceError>;
}
```

Expected layerstack service signature change:

```rust
impl LayerStackService {
    pub fn squash(
        &self,
        trace: Option<&OperationTrace>,
    ) -> Result<SquashLayerStackResult, LayerStackServiceError>;
}
```

No expected signature changes:

- `sandbox_runtime::dispatch_operation`;
- operation dispatch function pointer type;
- `CommandLaunchDriver::spawn`;
- `WorkspaceRuntimeService::create_workspace`;
- `CommandProcess::spawn`;
- `spawn_current_exe_ns_runner`;
- `ObservabilityStore::insert_trace`;
- `sandbox_protocol::Response`.

If the implementation tries to add compatibility aliases such as
`exec_command_with_trace` or `squash_with_trace`, prefer the hard signature
cutover inside this repo instead. Update call sites and tests directly.

## Candidate Span Decisions

| Candidate | Decision | Reason |
| --- | --- | --- |
| `command.exec.workspace.resolve` | Implement. | Lives in `CommandOperationService::exec_validated_command`; useful first split under `CommandOperationService::exec_command`. |
| `command.exec.workspace.resolve_existing_session` | Implement. | Existing-session path is in-process and caller-owned. |
| `command.exec.workspace.create_one_shot_session` | Implement. | One-shot workspace creation is a likely slow branch; caller can measure it without lower-crate trace dependencies. |
| `command.exec.workspace_runtime.create_workspace` | Defer. | The live call is below `WorkspaceSessionService`; first use create-one-shot-session as the boundary. Split later only if needed. |
| `command.exec.layerstack.snapshot_or_lease` | Defer. | The snapshot/lease call is inside the workspace crate and layerstack service crate. Avoid trace context below operation. |
| `command.exec.process.start` | Implement. | Captures workspace entry materialization plus launch-driver work without changing the launch-driver trait. |
| `command.exec.workspace.entry_materialize` | Defer. | First measure the broader process-start boundary; split later only if needed. |
| `command.exec.spawn.prepare_artifacts` | Defer. | Requires launch-driver signature churn and fake-driver updates; first measure the broader process-start boundary. |
| `command.exec.spawn.runtime_process` | Defer. | Requires launch-driver signature churn and reaches toward lower command-crate work; first measure the broader process-start boundary. |
| `command.exec.spawn.build_namespace_runner_request` | Defer. | Function is in the command crate; covered inclusively by process-start until a later launch-driver split is justified. |
| `command.exec.spawn.spawn_current_exe_ns_runner` | Defer. | Function is in the command crate and spawns the runner process; covered inclusively by process-start until a later launch-driver split is justified. |
| `layerstack.squash.open_stack` | Implement. | In-process caller-owned boundary in `LayerStackService::squash`. |
| `layerstack.squash.compact_stack` | Implement. | In-process caller-owned boundary around layerstack compaction. |
| `runner::run`, `run_setns`, `shell_exec::execute_shell` | Defer to Phase 4.5. | These are namespace-runner process internals, not Phase 3.5 request-local child spans. |

## Failure Behavior

- Disabled child keys call the original code path directly.
- Disabled child keys do not allocate spans, consume `call_index`, or alter
  parentage.
- Missing observability or missing `sandbox_id` still passes `None` to runtime
  dispatch and skips trace persistence.
- Store failures still do not fail or alter user operations.
- Enabled-key update failures or poisoned locks must not fail user operations.
- Unknown future config/operator strings are ignored until they map to exported
  constants in a later config phase.
- Panics keep the existing Phase 3 behavior: `SpanGuard::drop` records `panic`
  during unwind, but daemon dispatch does not add a new `catch_unwind` path in
  Phase 3.5.
- Child spans must not store command output, transcript contents, environment
  values, shell text, or response JSON.

## LOC Budget

Target from the parent spec:

```text
crates/sandbox-runtime non-test LOC: 60-130
```

Phase 3.5 should prefer the lower half of this range. Expected runtime
non-test split:

```text
span key/enabled-set additions                20-40
measure_if-style runtime API                  10-20
selected child span call-site wiring          20-50
tests and small exports as needed
```

Daemon enabled-key creation/update is outside the `crates/sandbox-runtime` LOC
budget, but it should stay narrow: one in-memory set, one snapshot method, one
completed-trace update method, and focused tests.

The runtime budget should stop and revise if the implementation needs more than
130 non-test LOC under `crates/sandbox-runtime`, or if it requires:

- a new schema;
- a background worker;
- a profiler-like engine;
- lower-crate trace dependencies;
- large public API churn outside selected service signatures.

If the budget is tight, keep only:

- `SpanKey`;
- `OperationTrace::measure_if`;
- command exec workspace/process boundary spans;
- layerstack open/squash spans;
- daemon in-memory enabled-key set.

Defer config enablement, TTL/decay, and lower-crate splits.

## Verification Plan

Focused checks:

```sh
cargo fmt --check
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime operation_trace
cargo check -p sandbox-daemon --tests
cargo test -p sandbox-daemon observability
cargo check -p sandbox-observability --tests
cargo test -p sandbox-observability
```

Required behavior tests:

- disabled span keys do not record child spans;
- enabled span keys record child spans with correct parentage under Phase 3
  parent spans;
- disabled keys do not consume `call_index`;
- call index ordering remains stable with mixed enabled and disabled child
  keys;
- existing Phase 3 coarse spans still appear;
- daemon can create a trace with enabled keys without changing operation
  responses;
- slow completed command traces enable command child keys for future requests;
- slow completed layerstack traces enable layerstack child keys for future
  requests;
- trace persistence uses existing `TraceRecord`, `SpanRecord`, and
  `ObservabilityStore::insert_trace`;
- response-derived operation errors attach to the appropriate Phase 3 coarse span,
  not to the deepest child span by default;
- no new schema migration is required;
- runtime tests do not import `sandbox-observability`;
- first-pass command process-start coverage does not change
  `CommandLaunchDriver::spawn`;
- missing `sandbox_id` still disables trace persistence without failing the
  request;
- observability store failures still do not fail user operations;
- no command output or transcript content appears in trace or span rows;
- no cross-process runner internals appear as Phase 3.5 child spans.

## Completion Criteria

Phase 3.5 is complete when:

- `OperationTrace` carries immutable request-local enabled keys;
- `SpanKey` and the first-pass span-key constants are exported from
  `sandbox-runtime/operation`;
- enabled child keys record spans through `measure_if`;
- disabled child keys call through without span rows or call-index changes;
- daemon dispatch constructs `OperationTrace` with enabled keys when
  observability is enabled;
- daemon-local enabled keys update from completed Phase 3 parent spans;
- selected command exec and layerstack child spans are wired;
- existing Phase 3 parent spans still appear;
- trace persistence still uses existing `traces` and `spans`;
- daemon response-error attribution does not mark child spans failed solely
  because they have the largest `call_index`;
- `sandbox_protocol::Response` remains unchanged;
- runtime crates still do not depend on `sandbox-observability` or SQLite;
- the verification plan passes.

## Open Questions

- The current checkout has Phase 3 implementation files in the worktree. Before
  implementing Phase 3.5, confirm those Phase 3 changes are intended to be the
  base and are not partial local work that should be amended first.
- The first dynamic approach uses a simple parent-span duration threshold. The
  exact threshold should be validated with local traces after Phase 3 is
  exercised under real command and layerstack workloads.
- If `command.exec.workspace.create_one_shot_session` remains too
  broad after Phase 3.5, a later narrow caller-owned
  `command.exec.workspace_runtime.create_workspace` span can be added in the
  operation crate without passing trace context into the workspace crate.
