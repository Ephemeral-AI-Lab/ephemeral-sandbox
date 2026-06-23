# Spec Authoring Prompt: Phase 3 Request Method Traces

Use this prompt to create a full implementation spec at:

```text
/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os/docs/observability/phase-3-request-method-traces.md
```

You are an architecture spec author. Your job is to write a concrete,
implementation-ready Phase 3 spec for coarse request method traces.

Do not implement code. Do not create review findings unless the live code makes
the Phase 3 spec impossible to write. Treat docs as proposals and live code as
the source of truth.

## Required Reading

Read these docs first:

```text
docs/observability/sandbox-observability.md
docs/observability/phase-1-observability-foundation.md
docs/observability/phase-2-runtime-snapshots.md
```

Then inspect live code, not just docs:

```text
crates/sandbox-protocol/src/request.rs
crates/sandbox-protocol/src/response.rs
crates/sandbox-runtime/operation/src/lib.rs
crates/sandbox-runtime/operation/src/operation.rs
crates/sandbox-runtime/operation/src/services.rs
crates/sandbox-runtime/operation/src/observability.rs
crates/sandbox-runtime/operation/src/command/service/impls/mod.rs
crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs
crates/sandbox-runtime/operation/src/command/service/impls/write_command_stdin.rs
crates/sandbox-runtime/operation/src/command/service/impls/read_command_lines.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/mod.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs
crates/sandbox-runtime/operation/src/command/service/core.rs
crates/sandbox-runtime/operation/src/command/service/process_store.rs
crates/sandbox-daemon/src/server/runtime.rs
crates/sandbox-daemon/src/server/dispatch.rs
crates/sandbox-daemon/src/observability/mod.rs
crates/sandbox-daemon/src/observability/service.rs
crates/sandbox-observability/src/records.rs
crates/sandbox-observability/src/store.rs
crates/sandbox-observability/tests/schema.rs
```

Use `rg` for call paths and names. Verify the current signatures instead of
assuming the docs are current.

## Phase 3 Scope

Phase 3 implements coarse request method traces only:

- create one request-local `OperationTrace` at daemon dispatch only when daemon
  observability is enabled and `sandbox_id` is present;
- pass optional trace context into runtime operation dispatch;
- add the automatic root `dispatch_operation` span;
- add one public service-method span for:
  - `exec_command`;
  - `write_command_stdin`;
  - `read_command_lines`;
  - `squash`;
- persist completed request traces and spans through daemon-owned
  observability storage;
- keep `sandbox_protocol::Response` unchanged.

Phase 3 must not implement:

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
- daemon decode-error trace records;
- trace hierarchy columns or indexes;
- response envelopes such as `{ result, meta }`;
- runtime SQLite writes;
- a `sandbox-observability` dependency from `sandbox-runtime`.

## Required Current-State Grounding

The spec must include a "Current Repo Grounding" section that confirms:

- whether Phase 2 snapshot code is present and what pieces are still dirty or
  incomplete;
- the current shape of `SandboxDaemonServer`, including any existing
  `DaemonObservability` field;
- the current daemon dispatch flow around `tokio::task::spawn_blocking`;
- the current `OperationEntry` and `dispatch_operation` signatures;
- the current public runtime operation entries;
- the current Phase 1/2 `TraceRecord`, `SpanRecord`, and `insert_trace` shape;
- whether `traces` currently has `workspace_id` and `command_session_id`, and
  that Phase 3 must not add them;
- that `Request` already carries `request_id`, `op`, `scope`, and `args`;
- that command output and transcripts stay outside observability storage.

Use exact file paths and current symbol names.

## Required Architecture Decisions

The spec must make these decisions explicit.

### Trace Context Ownership

Define `OperationTrace` as runtime-side request span collector that records
timing, nesting, and call order but does not know SQLite, daemon paths,
`sandbox-observability` record types, response JSON, `sandbox_id`, `request_id`,
operation, workspace hierarchy, or command hierarchy.

Specify:

- runtime fields only for monotonic start time, Unix start time, active parent
  stack, completed spans, and stable `call_index`;
- how daemon maps `trace_id = "request:" + request_id`;
- how daemon maps storage `span_id` from `trace_id` plus runtime `call_index`;
- that the implementation should pass `Option<&OperationTrace>`, not a disabled
  trace object and not `&mut OperationTrace`;
- `enter` for scope spans;
- `measure` for one call or expression;
- how spans finish on early return, panic unwind, or normal return;
- how daemon derives errors once from projected response JSON without changing
  operation response payloads;
- how the trace is completed before daemon persistence.

### Dispatch Boundary

Specify the signature changes needed in:

```text
crates/sandbox-runtime/operation/src/operation.rs
crates/sandbox-runtime/operation/src/command/service/impls/mod.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/mod.rs
crates/sandbox-runtime/operation/src/command/service/impls/*.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs
crates/sandbox-daemon/src/server/dispatch.rs
```

The spec must show the expected new function-pointer and dispatch shape in Rust
pseudocode. It must also explain how unknown operations and parse errors should
still get useful request/root spans without changing response payloads.

### Selected Phase 3 Spans

Use this coarse first-pass span policy:

```text
dispatch_operation
<operation>::dispatch
```

For `exec_command`, add only:

```text
CommandOperationService::exec_command
```

For `write_command_stdin`, add only:

```text
CommandOperationService::write_command_stdin
```

For `read_command_lines`, add only:

```text
CommandOperationService::read_command_lines
```

For `squash`, add only:

```text
LayerStackService::squash
```

The spec must explicitly defer these helper or lower-level spans:

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

Explain that Phase 3 spans are inclusive timings. For example,
`CommandOperationService::exec_command` includes workspace resolution, admission,
command process start, watcher launch, and initial yield. Helper spans move to
Phase 3.5 only after observed Phase 3 traces justify the split.

### Storage Migration

Specify that Phase 3 requires no schema migration. The live Phase 1 schema
already has `traces`, `spans`, `TraceRecord`, `SpanRecord`, and
`ObservabilityStore::insert_trace`.

Do not add `traces.workspace_id`, `traces.command_session_id`,
`idx_traces_workspace_time`, `idx_traces_command_time`, `trace_links`,
`origin_request_id`, `correlation_kind`, `correlation_id`, or `async_name` in
Phase 3.

### Daemon Persistence

Specify how `SandboxDaemonServer::dispatch_request` should:

- create the request trace only when observability is enabled and `sandbox_id`
  is available;
- pass `trace.as_ref()` into `sandbox_runtime::dispatch_operation`;
- project the operation response exactly as today;
- persist the completed request trace and spans after response projection;
- ignore or record observability write failures without changing the user
  operation response;
- continue to trigger Phase 2 snapshot collection after requests.

The daemon should map runtime trace completion into `TraceRecord` and
`SpanRecord`. Runtime must not import `sandbox-observability`.

## Required Spec Structure

Write `docs/observability/phase-3-request-method-traces.md` with this shape:

```text
# Phase 3 Request Method Traces

Status: draft implementation spec

Parent spec
Builds on

Exact Goal
Current Repo Grounding
Non-Goals
Architecture
  Runtime Trace Context
  Dispatch Boundary
  Daemon Persistence
  Storage Migration
  Selected Span Policy
  Phase 3.5 / Phase 4 / Phase 4.5 Boundaries
Detailed File Plan
Expected Struct and Signature Changes
Failure Policy
LOC Budget
Verification Plan
Completion Criteria
Open Questions
```

## Detailed File Plan Requirements

The spec must name expected files and keep the structure small. Use the existing
crate layout.

Expected additions or edits:

```text
crates/sandbox-runtime/operation/src/observability.rs
crates/sandbox-runtime/operation/src/lib.rs
crates/sandbox-runtime/operation/src/operation.rs
crates/sandbox-runtime/operation/src/command/service/impls/mod.rs
crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs
crates/sandbox-runtime/operation/src/command/service/impls/write_command_stdin.rs
crates/sandbox-runtime/operation/src/command/service/impls/read_command_lines.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/mod.rs
crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs
crates/sandbox-daemon/src/server/dispatch.rs
crates/sandbox-daemon/src/observability/service.rs
```

Do not split `observability.rs` into a module directory unless the final
implementation becomes hard to read. Do not add production storage files or a
daemon `observability/trace.rs` mapper unless the mapper demonstrably outgrows
`service.rs`.

Do not add a new crate. Do not add manager files.

## LOC Budget

The Phase 3 spec must include a runtime non-test LOC budget and explain where
the cost goes.

Use this target:

```text
crates/sandbox-runtime non-test LOC: 70-120
```

The implementation should aim for `75-95` runtime non-test LOC. Treat `120` as
the hard stop that forces scope reduction.

Expected split:

```text
OperationTrace + span guard/types            45-65
dispatch boundary plumbing                   15-20
selected operation wrapper spans             10-15
exports/module movement, if needed            0-5
```

If the spec predicts more than 120 runtime non-test LOC, it must stop and revise
the boundary. The usual mistake is implementing Phase 3.5 spans, disabled/no-op
trace state, storage-shaped DTOs, public service signature churn, module
ceremony, or lower-crate tracing inside Phase 3.

## Verification Plan Requirements

Include focused checks:

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

- no Phase 3 schema migration or trace hierarchy indexes are added;
- synthetic request trace persists all spans under one `trace_id`;
- span `call_index` ordering is stable;
- nested runtime spans map to the correct storage `parent_span_id`;
- early returns close active spans;
- operation errors still persist trace status without changing response shape;
- missing `sandbox_id` disables persistence without failing requests;
- observability store failures do not change operation responses;
- runtime tests do not import `sandbox-observability`;
- public service method signatures do not change;
- selected operation traces contain only root, operation dispatch, and one public
  service-method span.

## Output Rules

- Write the complete spec, not a summary.
- Use exact file paths and current symbol names.
- Separate live-code facts from design decisions.
- Keep Phase 3 narrower than Phase 3.5, Phase 4, and Phase 4.5.
- Do not recommend aliases, compatibility shims, fallback response shapes, or
  response-envelope migrations.
- Do not make command transcripts or command output part of observability.
- Do not require external observability services.
- If live code has drifted enough that this prompt is wrong, document the drift
  in an "Open Questions" section and keep the proposed correction minimal.
