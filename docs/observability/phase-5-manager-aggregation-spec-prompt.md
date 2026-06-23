# Spec Authoring Prompt: Phase 5 Manager Observability Aggregation

Use this prompt to create a full implementation spec at:

```text
/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os/docs/observability/phase-5-manager-aggregation.md
```

You are an architecture spec author. Your job is to write a concrete,
implementation-ready Phase 5 spec for daemon snapshot queries and manager
aggregation across many sandbox daemons.

Do not implement code. Do not create review findings unless the live code makes
the Phase 5 spec impossible to write. Treat docs as proposals and live code as
the source of truth.

Bias hard toward a bounded pull model. Phase 5 must not turn observability into a
load-bearing control-plane dependency, a manager-side database, a raw SQL access
surface, or a global telemetry pipeline.

## Required Reading

Read these docs first:

```text
docs/observability/sandbox-observability.md
docs/observability/phase-1-observability-foundation.md
docs/observability/phase-2-runtime-snapshots.md
docs/observability/phase-3-request-method-traces.md
docs/observability/phase-3-5-targeted-deep-request-spans.md
docs/observability/phase-4-async-method-traces.md
docs/observability/phase-4-5-namespace-runner-traces.md
```

Then inspect live code, not just docs:

```text
crates/sandbox-protocol/src/request.rs
crates/sandbox-protocol/src/response.rs
crates/sandbox-protocol/src/scope.rs
crates/sandbox-protocol/src/catalog.rs
crates/sandbox-manager/src/model.rs
crates/sandbox-manager/src/store.rs
crates/sandbox-manager/src/daemon_client.rs
crates/sandbox-manager/src/router/dispatch.rs
crates/sandbox-manager/src/router/forward.rs
crates/sandbox-manager/src/operation/dispatch.rs
crates/sandbox-manager/src/operation/impls/management/mod.rs
crates/sandbox-daemon/src/server/runtime.rs
crates/sandbox-daemon/src/server/dispatch.rs
crates/sandbox-daemon/src/observability/service.rs
crates/sandbox-daemon/src/observability/cgroup.rs
crates/sandbox-daemon/src/observability/disk.rs
crates/sandbox-observability/src/paths.rs
crates/sandbox-observability/src/records.rs
crates/sandbox-observability/src/store.rs
crates/sandbox-daemon/tests/unit/observability.rs
crates/sandbox-manager/tests/manager_router.rs
crates/sandbox-manager/tests/manager_core.rs
crates/sandbox-observability/tests/schema.rs
```

Use `rg` for call paths and names. Verify current signatures instead of assuming
the docs are current.

## Phase 5 Scope

Phase 5 adds query APIs over existing local observability state:

- daemon API `get_observability_snapshot`;
- manager API `get_observability_tree`;
- manager fan-out across ready sandbox daemons;
- typed snapshot DTO projection;
- bounded partial-failure handling;
- bounded resource and recent-trace query options.

The daemon must run inside the sandbox. The authoritative observability store is:

```text
/eos/runtime/daemon/observability/observability.sqlite
```

The manager may hold endpoint, proxy, auth, and lifecycle metadata so it can
reach each daemon. The manager must not own, open, copy, mirror, compact,
migrate, or read daemon SQLite files.

The final spec must print this exact rollout-budget line in an "Expected LOC"
section:

```text
Expected `crates/sandbox-runtime` change: 0 non-test LOC.
```

If the proposed design needs any production change in `crates/sandbox-runtime`,
reject the architecture and simplify it before writing the detailed plan.

Phase 5 must not implement:

- new runtime snapshot producers;
- new runtime tracing spans;
- SQLite reads or writes from `sandbox-runtime`;
- a `sandbox-observability` dependency from `sandbox-runtime`;
- manager-side `observability.sqlite`;
- manager-side mirror tables or cache databases;
- raw SQL query APIs;
- `get_method_trace`, `list_method_traces`, or `get_resource_samples` drilldown
  APIs unless the parent spec is explicitly revised;
- Prometheus, Grafana, Loki, Tempo, OTLP, or log export;
- command transcript or command output ingestion;
- response envelopes such as `{ result, meta }`;
- public command response-shape changes;
- a global event bus or streaming telemetry protocol.

## Required Current-State Grounding

The spec must include a "Current Repo Grounding" section that confirms:

- that the daemon runs inside the sandbox and the intended observability store is
  under `/eos/runtime/daemon/observability/observability.sqlite`;
- how `ObservabilityPaths` currently derives `observability.sqlite` from
  `ServerConfig.socket_path.parent()`;
- how `DaemonObservability::from_config` enables or disables observability;
- how `SandboxDaemonServer::trigger_observability_collection` currently writes
  daemon-local snapshots best-effort;
- which store methods exist for reading current snapshot rows, recent traces,
  resource samples, namespace execution rows, or test-only projections;
- whether `ObservabilityStore` currently has production read APIs, and exactly
  which new read APIs Phase 5 needs;
- the current request/response shape in `sandbox-protocol`, including the rule
  that `Response::ok(result)` returns the result directly;
- how `SandboxManagerRouter` distinguishes manager-owned operations from
  sandbox-scoped forwarded daemon operations;
- how `SandboxDaemonClient::invoke` and `SandboxDaemonEndpoint` currently model
  daemon transport;
- how `SandboxStore` lists or inspects ready sandbox records;
- how manager operations are registered in `ManagerOperationEntry` and the
  management operation family;
- whether the daemon currently has a daemon-owned operation registry or only
  forwards to `sandbox_runtime::dispatch_operation`;
- how daemon dispatch validates sandbox scope before runtime dispatch;
- what tests currently exist for daemon observability, manager routing, manager
  core behavior, and store schema.

Use exact file paths and current symbol names.

## Required Self-Critical Architecture Check

Before the detailed file plan, the spec must include a self-critical
architecture check. This is not a generic pros/cons section. It must challenge
the proposed design and either simplify it or explain why the extra complexity
is necessary.

The check must answer these questions directly.

### Load and Safety

- Does `get_observability_tree` remain summary-first rather than deep
  inspection?
- What is the maximum number of daemon calls issued concurrently?
- What timeout applies per daemon request?
- What happens when one daemon is slow, unavailable, unauthorized, or returns
  malformed data?
- Can the manager return partial results with unavailable sandbox nodes instead
  of failing the whole tree?
- Does the daemon cap `trace_limit` and `resource_window_ms` regardless of
  caller input?
- Does the daemon return latest/current resources by default instead of resource
  history?
- Does the daemon reuse cached disk samples and avoid fresh expensive disk walks
  on every query?
- Can SQLite lock or read failures return bounded partial snapshot errors rather
  than failing user operations?

### Ownership Boundaries

- Does the daemon remain responsible for `/eos` storage reads, current runtime
  snapshot collection, trace/resource projection, and partial local errors?
- Does the manager remain responsible only for selecting ready sandboxes,
  contacting daemon endpoints, and aggregating typed DTOs?
- Does the manager avoid direct filesystem access to
  `/eos/runtime/daemon/observability/observability.sqlite`?
- Does the manager avoid a second observability cache database or mirror table?
- Does `sandbox-runtime` stay unchanged?
- Does Phase 5 avoid adding `sandbox-observability` types to manager public
  response DTOs if a stable API DTO layer is more appropriate?

### API Minimality

- Can Phase 5 be reduced to one daemon query and one manager aggregation query?
- Should `get_observability_snapshot` be a daemon-owned dispatch branch rather
  than a runtime operation? If the spec chooses a daemon-owned branch, explain
  how it is exposed through the existing daemon protocol/catalog/help surfaces.
- If adding a daemon operation registry is necessary, what is the smallest
  version that avoids broad runtime or manager refactors?
- Should manager aggregation call `SandboxDaemonClient::invoke` with a sandbox
  scoped `Request`, or does the transport layer need a narrower helper?
- Which DTO fields are required for the hierarchy, and which fields should be
  deferred until drilldown APIs exist?

If the self-critical check finds that a simpler design works, the spec must use
the simpler design.

## Required Architecture Decisions

The spec must make these decisions explicit.

### Daemon Query API

Define the daemon-owned operation:

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

The spec must define:

- the JSON argument shape;
- default values;
- daemon caps for `trace_limit` and `resource_window_ms`;
- behavior when `workspace_id` is unknown;
- behavior when observability is disabled;
- behavior when SQLite read fails but live runtime state can still be sampled;
- whether the query triggers a fresh snapshot collection, reuses current rows,
  or does both with rate limits;
- how bounded errors appear in the DTO without changing the response envelope;
- how command transcript content remains excluded.

### Manager Query API

Define the manager-owned operation:

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

The spec must define:

- how `sandbox_ids = None` selects ready sandboxes from the manager store;
- how `sandbox_ids = Some(..)` handles missing, stopped, failed, or duplicate
  ids;
- how the manager constructs per-daemon `get_observability_snapshot` requests;
- how manager uses `SandboxDaemonClient::invoke` or the smallest necessary
  transport helper;
- concurrency and timeout policy;
- deterministic output ordering;
- unavailable sandbox node shape;
- whether manager returns partial success with embedded unavailable nodes or a
  top-level error for all-daemon failure;
- how auth and endpoint errors are represented without leaking secrets.

### DTO Shape

Use typed DTOs. SQLite rows are implementation details.

The spec must define or reference DTOs for:

```text
SandboxSnapshot
WorkspaceSnapshot
ExecutionSnapshot
NamespaceExecutionSnapshot
ResourceSnapshot
TraceSummary
UnavailableSandboxSnapshot
SnapshotPartialError
```

The DTO design must preserve the display hierarchy:

```text
sandbox_id
  state
  resources
  workspace_id
    state
    resources
    active executions
    active commands (filtered from active executions)
    active namespace executions
    recent traces
```

Do not expose raw SQLite column names where a stable API name is clearer. Do not
include command output, stdin, environment variables, raw transcript rows, full
file lists, or unbounded error strings.

### Store Read APIs

The spec must decide the smallest production read surface in
`crates/sandbox-observability`.

Prefer bounded helpers such as:

```text
load_sandbox_snapshot(sandbox_id)
load_workspace_snapshots(sandbox_id, workspace_id?)
load_execution_snapshots(sandbox_id, workspace_id?)
load_namespace_execution_snapshots(sandbox_id, workspace_id?)
load_latest_resource_samples(sandbox_id, workspace_id?)
load_resource_samples(sandbox_id, workspace_id?, since_unix_ms, limit)
load_recent_trace_summaries(sandbox_id, workspace_id?, limit)
```

Reject a generic SQL query helper or public raw connection access. Explain which
indexes already support the reads and which minimal index additions, if any, are
needed.

### Daemon Dispatch Shape

The spec must decide how `get_observability_snapshot` is dispatched.

Preferred direction: daemon-owned query handling before or beside runtime
dispatch, because the query reads daemon-local `/eos` observability state and
must not become a `sandbox-runtime` operation.

The spec must show Rust-like pseudocode for:

```text
SandboxDaemonServer::dispatch_request
  validate daemon sandbox scope
  if request.op == "get_observability_snapshot":
      return daemon observability query response
  else:
      dispatch runtime operation as today
```

If the live catalog/help architecture requires a daemon operation catalog, the
spec must define the smallest catalog addition and explain why it is necessary.

### Manager Aggregation Shape

The spec must show Rust-like pseudocode for:

```text
dispatch get_observability_tree
  parse input and caps
  select sandbox records
  for each ready sandbox, with concurrency limit:
      call daemon get_observability_snapshot through SandboxDaemonClient
      map success to SandboxSnapshot
      map failure to unavailable sandbox snapshot
  return { sandboxes: ordered_snapshots }
```

The first implementation may be synchronous inside the existing manager
`spawn_blocking` dispatch if that matches the live manager operation model. If
the spec proposes async fan-out, it must explain the minimal change required to
the manager operation dispatch architecture and why the complexity is worth it.

## Required File Plan

The spec must include a file-by-file plan. Include only files that need changes.

Consider these likely files:

```text
docs/observability/phase-5-manager-aggregation.md
crates/sandbox-protocol/src/catalog.rs
crates/sandbox-daemon/src/server/dispatch.rs
crates/sandbox-daemon/src/observability/service.rs
crates/sandbox-daemon/tests/unit/observability.rs
crates/sandbox-observability/src/store.rs
crates/sandbox-observability/tests/schema.rs
crates/sandbox-manager/src/operation/impls/management/mod.rs
crates/sandbox-manager/src/operation/impls/management/get_observability_tree.rs
crates/sandbox-manager/src/router/dispatch.rs
crates/sandbox-manager/tests/manager_core.rs
crates/sandbox-manager/tests/manager_router.rs
```

The file plan must also list files that should not change:

```text
crates/sandbox-runtime/operation/src/**
crates/sandbox-runtime/*/Cargo.toml
command transcript code
namespace-runner child process code
```

## Required Tests

The spec must include focused tests for:

- daemon snapshot query returns a bounded typed snapshot from existing
  observability rows;
- daemon snapshot query works when `include_recent_traces = false`;
- daemon caps excessive `trace_limit` and `resource_window_ms`;
- daemon returns an unavailable or partial snapshot when observability is
  disabled or SQLite reads fail;
- daemon query does not include transcript content or command output;
- manager `get_observability_tree` selects ready sandboxes;
- manager maps stopped/failed/missing/unreachable daemons to unavailable nodes
  according to the chosen API rule;
- manager uses daemon client fan-out instead of opening SQLite;
- manager preserves deterministic result ordering;
- manager does not leak auth tokens in responses or errors;
- no `sandbox-runtime` dependency on `sandbox-observability` is introduced.

## Verification Commands

The spec must include verification commands. Start with:

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

If a command is not appropriate after inspecting the live workspace, revise it
and explain why in the spec.

## Completion Checklist

The spec must end with a checklist.

Required checklist items:

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
