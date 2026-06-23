/goal Implement Phase 3 request method traces in `/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os`, keeping the total implementation under 3800 changed LOC and keeping `crates/sandbox-runtime` non-test additions within 70-120 LOC, preferably 75-95.

You are implementing the existing Phase 3 spec. Do not redesign it unless live
code has drifted enough that the spec cannot compile. Prefer deletion, narrow
surface area, and direct mapping over abstraction.

## Required Reading

Read the implementation spec first:

```text
docs/observability/phase-3-request-method-traces.md
```

Then inspect live code before editing:

```text
crates/sandbox-protocol/src/request.rs
crates/sandbox-protocol/src/response.rs
crates/sandbox-runtime/operation/src/lib.rs
crates/sandbox-runtime/operation/src/operation.rs
crates/sandbox-runtime/operation/src/observability.rs
crates/sandbox-runtime/operation/src/services.rs
crates/sandbox-runtime/operation/src/command/service/impls/mod.rs
crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs
crates/sandbox-runtime/operation/src/command/service/impls/write_command_stdin.rs
crates/sandbox-runtime/operation/src/command/service/impls/read_command_lines.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/mod.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs
crates/sandbox-daemon/src/server/runtime.rs
crates/sandbox-daemon/src/server/dispatch.rs
crates/sandbox-daemon/src/observability/service.rs
crates/sandbox-observability/src/records.rs
crates/sandbox-observability/src/store.rs
crates/sandbox-observability/tests/schema.rs
```

Run `git status --short` first. The worktree may contain unrelated user
changes; do not revert them.

## Hard Scope

Implement only completed request method traces for the daemon-owned runtime
operation path:

- create `OperationTrace` only when daemon observability is enabled and
  `sandbox_id` is present;
- pass `Option<&OperationTrace>` through runtime dispatch;
- record `dispatch_operation`;
- record matched `<operation>::dispatch`;
- record exactly one public service-method span for each selected operation:
  `CommandOperationService::exec_command`,
  `CommandOperationService::write_command_stdin`,
  `CommandOperationService::read_command_lines`,
  `LayerStackService::squash`;
- persist completed request traces and spans through daemon-owned
  `ObservabilityStore::insert_trace`;
- keep `sandbox_protocol::Response` unchanged.

Do not implement:

- disabled/no-op trace objects;
- public service method signature changes;
- helper spans such as `resolve_exec_workspace`, `start_command_process`,
  `initial_exec_yield`, `write_or_cancel`, `wait_for_command_yield`, or
  `read_transcript_window`;
- Phase 3.5, Phase 4, or Phase 4.5 traces;
- async trace links or namespace-runner/shell-exec traces;
- daemon decode-error traces;
- manager aggregation or daemon query APIs;
- command output or transcript ingestion;
- `{ result, meta }` response envelopes;
- production storage migrations;
- `traces.workspace_id`, `traces.command_session_id`, or hierarchy indexes;
- runtime dependency on `sandbox-observability`, `rusqlite`, daemon paths, or
  store/record types;
- new crates or compatibility shims.

## Runtime Implementation

Keep runtime tracing in `crates/sandbox-runtime/operation/src/observability.rs`
unless the file becomes genuinely hard to read. Add only:

- `OperationTrace`;
- `SpanGuard`;
- `CompletedOperationTrace`;
- `CompletedOperationSpan`;
- a tiny optional measurement helper if it keeps dispatch code clean.

Runtime trace state should store only:

- monotonic request start time;
- Unix request start time in milliseconds;
- active parent stack by `call_index`;
- completed spans;
- next stable `call_index`.

Runtime must not store trace id, sandbox id, request id, operation, workspace id,
command session id, response JSON, terminal trace status, terminal error kind, or
terminal error message.

Use `RefCell<TraceState>` for request-local interior mutability. Do not use
`Arc<Mutex<_>>`. `SpanGuard::drop` must close spans on normal return, early
return, and panic unwind. Runtime span names and runtime span statuses should be
`&'static str`; daemon mapping can allocate storage strings.

Expected DTO shape:

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

## Dispatch Boundary

Change runtime dispatch signatures to accept `Option<&OperationTrace>`:

```rust
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response;
```

Update `OperationEntry` dispatch pointers and selected operation dispatch
functions accordingly. Do not change `CommandOperationService` or
`LayerStackService` public method signatures.

In `dispatch_operation`, wrap the whole dispatch lookup in `dispatch_operation`.
If an entry matches, wrap the entry call in `<operation>::dispatch`. Unknown ops
get only `dispatch_operation`.

In selected operation dispatch wrappers, parse input as today, then wrap only the
single public service call in the public service-method span. Parse errors should
get `dispatch_operation` and `<operation>::dispatch`, but no `parse_input` span.

## Daemon Persistence

Modify `crates/sandbox-daemon/src/server/dispatch.rs` so request handling:

- creates `Some(OperationTrace::new())` only when `self.observability` and a
  non-empty `sandbox_id` exist;
- otherwise passes `None`;
- calls `sandbox_runtime::dispatch_operation(&operations, &request, trace.as_ref())`;
- projects the response with `into_json_value()` exactly as today;
- persists the completed trace after response projection and before triggering
  Phase 2 snapshot collection;
- performs trace persistence inside the existing `spawn_blocking` work item so
  SQLite work stays off the async continuation path;
- ignores trace write failures and returns the same operation response.

Add a daemon-owned persistence method in
`crates/sandbox-daemon/src/observability/service.rs`:

```rust
pub(crate) fn insert_completed_operation_trace(
    &self,
    sandbox_id: String,
    request_id: String,
    operation: String,
    response: &serde_json::Value,
    trace: CompletedOperationTrace,
) -> Result<(), StoreError>;
```

Daemon mapping rules:

- `trace_id = format!("request:{request_id}")`;
- storage span id is `format!("{trace_id}:span:{call_index}")`;
- storage parent id is derived from `parent_call_index`;
- trace `kind` is `"request"`;
- trace status/error metadata is derived once from projected response JSON;
- successful responses are `ok`;
- top-level `error.kind` and `error.message` produce trace `error` status and
  bounded error fields;
- for fault responses, mark the deepest completed span with the same bounded
  error metadata.

Do not move `TraceRecord`, `SpanRecord`, `ObservabilityStore`, SQLite handles,
or daemon paths into runtime.

## Storage

Do not add a production migration. The existing `traces`, `spans`,
`TraceRecord`, `SpanRecord`, and `ObservabilityStore::insert_trace` are enough.

Add a hidden/test-only trace read helper only if daemon tests cannot reasonably
assert trace rows with existing helpers or direct SQLite reads. Do not expose a
product query API.

## Tests

Add focused tests without broad rewrites.

Runtime tests should cover:

- call index ordering;
- nesting and `parent_call_index`;
- early-return guard closure;
- panic-unwind guard closure status;
- selected span set if practical;
- no `sandbox-observability` imports.

Daemon tests should cover:

- successful trace mapping and persistence;
- unknown operation trace persistence;
- operation argument parse error trace persistence;
- operation/service error trace persistence;
- missing `sandbox_id` disables trace persistence without failing the request;
- observability store failure does not alter operation responses;
- command output and transcript content do not appear in trace/span rows.

Storage/schema tests should assert:

- synthetic trace insertion still works;
- no Phase 3 migration, hierarchy columns, or hierarchy indexes are added.

## LOC and Diff Discipline

Keep total changed LOC under 3800. Keep runtime non-test additions within
70-120 LOC, preferably 75-95. Before finalizing, report:

```sh
git diff --numstat -- crates/sandbox-runtime/operation/src
git diff --stat
```

If runtime additions exceed 120 non-test LOC, stop and remove scope before
continuing. The usual causes are disabled trace state, helper spans,
storage-shaped DTOs, service signature churn, module splits, or Phase 4 links.

## Verification

Run these checks, or state exactly why a check could not be run:

```sh
cargo fmt --check
cargo check -p sandbox-observability --tests
cargo test -p sandbox-observability
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime operation_trace
cargo check -p sandbox-daemon --tests
cargo test -p sandbox-daemon observability
git diff --check
```

Also run targeted boundary scans:

```sh
rg -n "sandbox-observability|rusqlite|ObservabilityStore|TraceRecord|SpanRecord" crates/sandbox-runtime/operation/src crates/sandbox-runtime/operation/Cargo.toml
rg -n "workspace_id|command_session_id|idx_traces_workspace_time|idx_traces_command_time|trace_links|origin_request_id|correlation_kind|async_name" crates/sandbox-observability/src crates/sandbox-observability/tests
rg -n "resolve_exec_workspace|start_command_process|initial_exec_yield|write_or_cancel|read_transcript_window|runner::run|shell_exec" crates/sandbox-runtime/operation/src/observability.rs crates/sandbox-runtime/operation/src/operation.rs crates/sandbox-runtime/operation/src/command/service/impls crates/sandbox-runtime/operation/src/layerstack/service/impls
```

Expected scan result: runtime may mention `OperationTrace` and local trace DTOs,
but must not import observability store/record/SQLite types; storage must not
add Phase 3 hierarchy or async-link schema; selected operation implementation
must not add helper or runner spans.

## Final Response

Report:

- files changed;
- runtime non-test LOC delta;
- total changed LOC/stat;
- verification commands and results;
- any deferred items or residual risk.

Do not claim success if any hard Phase 3 boundary was violated.
