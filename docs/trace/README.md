# Runtime Trace And Event Spec

Status: draft

This document specifies how sandbox runtime tracing should be introduced across
`sandbox-daemon`, `sandbox-runtime`, `sandbox-runtime-workspace`,
`sandbox-runtime-layerstack`, `sandbox-runtime-overlay`, and
`sandbox-runtime-namespace-process`.

The goal is to make runtime execution explainable from one correlated chain:

- what operation ran
- which workspace and command session it touched
- which workspace/layerstack/overlay phases ran
- how long each phase took
- what result or rejection happened
- which logs belong to the same operation

## Decision

Use the Rust `tracing` ecosystem in code. The production collection path is
OpenTelemetry export from each in-sandbox daemon to a host-local OpenTelemetry
Collector endpoint that is reachable from the sandbox.

```text
sandbox-daemon / sandbox-runtime code
  -> tracing spans and events
  -> tracing-subscriber
  -> OTLP exporter
  -> host OpenTelemetry Collector
  -> Tempo / Jaeger for traces
  -> Loki for logs
  -> Prometheus/Grafana for metrics
```

Code should depend on `tracing`, not directly on a vendor SDK. OTLP export is a
daemon/config concern.

Runtime crates emit spans/events only. They do not initialize subscribers,
choose exporters, own OTLP config, or provide a custom trace abstraction layer.

Do not implement automatic fallback between collection paths. A daemon
deployment chooses one active sink. If the configured exporter cannot deliver
telemetry, runtime protocol behavior must not switch to a secondary file,
stdout, or manager-RPC collection path.

## Non-Goals

- Do not replace command transcripts. `transcript.log` remains functional
  command output, not observability logging.
- Do not make traces part of correctness. Protocol results and typed errors stay
  authoritative.
- Do not revive layerstack-specific event object pipelines. `layerstack` should
  return structured publish results and errors; operation-level code should
  translate those facts into spans/events.
- Do not create a `sandbox-runtime-trace` crate.
- Do not add `crates/sandbox-runtime/operation/src/internal/telemetry.rs`.
  Runtime instrumentation should live inline at the existing semantic boundary
  call sites.
- Do not instrument every helper function by default. Instrument stable runtime
  boundaries and important internal phases first.
- Do not treat tracing as CPU profiling. Use `perf`, flamegraph, or pprof-style
  tooling for deep CPU hotspots.

## Concepts

**Span:** timed interval with parent/child relationship. Use spans for work that
has a meaningful duration, for example `runtime.exec_command`,
`workspace.create_session`, `layerstack.publish_changes`, or
`workspace.remount`.

**Event:** point-in-time fact attached to the current span. Use events for state
transitions, rejection reasons, selected results, and cleanup outcomes.

**Metric:** aggregate counter, gauge, or histogram. Metrics can be emitted
directly later or derived from spans/events in the collector.

**Result:** stable operation output returned to the caller. Results must remain
available even when tracing is disabled.

## Time Measurement

Trace timing is wall-clock latency, not CPU profiling.

Use three timing forms:

| Form | Meaning | Use For |
| --- | --- | --- |
| span duration | `span.end_time - span.start_time` | operation and phase latency |
| event timestamp | time when an event was emitted | ordering point-in-time facts |
| explicit phase timer | `std::time::Instant::now().elapsed()` | typed phase reports and stable result fields |

Span duration is the default for runtime boundaries such as
`runtime.exec_command`, `workspace.capture_changes`,
`layerstack.publish_changes`, and `workspace.remount`. Parent span duration is
inclusive only for work the live call path actually performs. For example,
`runtime.exec_command` includes workspace resolution or creation, command spawn,
initial yield, command finalization, process waits, locks, I/O waits, and
one-shot workspace destroy when that path owns the workspace. It must not show
workspace capture, layerstack publish, or remount as child spans unless the code
is intentionally changed so `exec_command` owns that work. Publish and remount
have their own operation spans.

Events should not be used as the primary duration mechanism. They are for
ordered facts such as `publish_occ_checked`, `remount_verified`, or
`cleanup_finished`. If an event represents a timed phase, include an explicit
`duration_ms` field.

Keep existing explicit phase timers as `Instant` measurements. For workspace
create, the setup phase timing map is internal to
`WorkspaceModeManager::initialize_handle`; emit those values into trace events
at that boundary without adding a public `CreateWorkspaceResult` field:

```text
event = workspace_create_phase_finished
phase = mount_overlay
duration_ms = 17
```

Existing command response timing fields, such as command wall time and total
command time, are typed command lifecycle fields. Do not use them as operation
latency. Operation latency belongs to span durations and metrics histograms.
Response timing fields should not be expanded to satisfy dashboards; once
callers no longer need them for workflow display, they can be simplified in a
separate API cleanup.

For async code, instrument the future so duration covers the future from poll
start to completion, including `.await` time. Use `#[instrument]` only where the
function boundary is already a stable diagnostic boundary; otherwise create an
explicit span and enter/instrument the relevant block or future.

When using `#[instrument]`, skip all request/input structs by default and record
only explicit safe fields. Do not auto-capture `sandbox_protocol::Request.args`,
command text, stdin text, command output, environment values, auth tokens, raw
workspace roots, raw cgroup paths, or raw layer paths.

Latency dashboards should use metrics histograms derived from span durations or
emitted directly. Tracing must not be treated as CPU profiling; use `perf`,
flamegraph, or pprof-style tooling for CPU hotspots.

## Initial Dependencies

Add these workspace dependencies when implementation starts:

```toml
tracing = { version = "0.1", features = ["attributes"] }
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
```

Runtime crates should depend only on `tracing` unless they have a concrete
reason to own subscriber behavior. `sandbox-daemon` owns `tracing-subscriber`
setup.

Add OpenTelemetry crates only when OTLP export is implemented. Select exact
compatible versions in one Cargo change, document the chosen transport feature,
and do not leave placeholders, wildcard `0.x` declarations, or mixed-generation
OTel crates in `Cargo.toml`:

Required crates are `tracing-opentelemetry`, `opentelemetry`,
`opentelemetry_sdk`, and `opentelemetry-otlp`; the implementation PR must show
their exact selected versions and the single selected OTLP transport feature in
the actual manifest diff.

Exact OpenTelemetry crate versions should be selected together because the Rust
OTel crates move in lockstep. The implementation must document the selected
feature set for the configured protocol (`http` or `grpc`) and avoid wildcard
`0.x` dependency declarations in `Cargo.toml`.

## Code Ownership

Keep the tracing ownership narrow:

| Area | Owns |
| --- | --- |
| `sandbox-daemon` | telemetry initialization, subscriber setup, dynamic `sandbox_id`, OTLP exporter |
| `sandbox-runtime` operation crate | inline spans/events in existing command, workspace, remount, layerstack wrappers |
| `sandbox-runtime-workspace` | inline spans/events at stable workspace service boundaries when needed |
| `sandbox-runtime-layerstack` | structured results/errors; no custom trace-event objects |
| `sandbox-runtime-overlay` | no tracing by default unless a low-level mount syscall cannot be diagnosed from callers |
| `sandbox-config` | typed config schemas only |

Do not add a central runtime trace module for field-name constants or wrapper
macros. Prefer direct `tracing` macros at the call site. If repetition becomes a
real problem later, first prove it with duplicated call-site code in review
before adding any abstraction.

## Runtime Boundaries

The first pass should instrument these boundaries:

```text
sandbox-daemon
  server.dispatch_request
    sandbox_runtime::dispatch_operation
      runtime.exec_command
        workspace.resolve_session | workspace.create_session
        command.spawn
        command.wait_initial_yield
        command.finalize
        workspace.destroy_session  # one-shot command workspaces only

      workspace.capture_changes
        overlay.capture_upperdir
      layerstack.publish_changes
      workspace.remount

      runtime.write_command_stdin
      runtime.read_command_lines
      cgroup_monitor.anomaly
      cgroup_monitor.final_summary
```

`sandbox-runtime-overlay` should stay a low-level mechanism crate. Prefer
spans/events around calls into overlay from workspace/namespace-process unless a
specific low-level mount step cannot be diagnosed from the caller.

## Trace Identity

Every daemon request span must carry:

| Field | Source |
| --- | --- |
| `request_id` | `sandbox_protocol::Request.request_id` |
| `operation` | `sandbox_protocol::Request.op` |
| `scope` | request scope, redacted if needed |
| `sandbox_id` | daemon runtime identity loaded at daemon startup |
| `scope_sandbox_id` | request scope value when the protocol carries one |

`sandbox_id` is the canonical runtime identity. It is not pre-known for
`manager.create_sandbox`; the runtime creates the sandbox and returns the
resulting id. Traces emitted before that point must use `request_id` or an
explicit `creation_correlation_id`, then record an event when the sandbox id is
assigned:

```text
manager.create_sandbox
  request_id = ...
  creation_correlation_id = ...
  event sandbox.created { sandbox_id }
```

After creation succeeds, the manager starts the daemon with a dynamic identity
source, for example an environment variable, daemon argument, or generated
identity file. The static telemetry YAML should not contain `sandbox_id` or a
separate static sandbox identity. At startup, the daemon reads the sandbox id
once and attaches it to all root spans and OpenTelemetry resource attributes.

Runtime child spans should add the IDs they own:

| Field | Owner |
| --- | --- |
| `workspace_session_id` | workspace session service |
| `command_session_id` | command service |
| `lease_id` | workspace/layerstack |
| `manifest_version` | workspace/layerstack |
| `root_token` | optional keyed/bounded surrogate for workspace/layerstack correlation; never a metric label |
| `cgroup_path_present` | workspace/command |

Paths should be recorded carefully. Prefer booleans, counts, hashes, redacted
path classes, and stable IDs over full paths. Do not emit raw host paths, raw
workspace roots, raw cgroup paths, raw layer paths, raw upper/work dirs,
transcript/artifact paths, raw PIDs, or path-derived IDs in any telemetry mode,
including local/test JSON. Local/test fixtures may include sentinel values only
as inputs for negative assertions that prove those values do not appear in
telemetry.

## Span And Semantic Event Names

Use dot-separated stable names. The cgroup monitor entries are internal event
boundaries, not spans around public read operations:

```text
daemon.request
runtime.exec_command
runtime.write_command_stdin
runtime.read_command_lines
workspace.create_session
workspace.destroy_session
workspace.capture_changes
workspace.remount
overlay.capture_upperdir
layerstack.publish_changes
command.spawn
command.wait_initial_yield
command.finalize
cgroup_monitor.anomaly
cgroup_monitor.final_summary
```

Avoid names that mirror temporary private helper files unless the helper is a
stable diagnostic boundary. For the initial semantic-span rollout, do not add
spans named after private layerstack helpers such as `plan_publish`,
`validate_source_paths`, or manifest commit internals; emit structured
publish/OCC facts as telemetry stats from the operation-level publish wrapper
instead.

## Event Names

Use snake-case message names so JSON logs and OTel event names are easy to
query:

```text
request_received
request_finished
workspace_created
workspace_create_phase_finished
workspace_destroyed
command_started
command_exited
command_finalization_started
command_finalization_finished
overlay_capture_finished
publish_started
publish_route_planned
publish_occ_checked
publish_finished
publish_rejected
remount_started
remount_verified
remount_failed
cgroup_monitor_anomaly
cgroup_final_summary_recorded
cleanup_finished
```

## Result Fields To Emit

Emit these fields as span fields or events while keeping them in typed results
where they already exist:

| Area | Fields |
| --- | --- |
| command | `status`, `exit_code`, `cancelled`, `timed_out` |
| workspace create | phase name, `duration_ms`, profile |
| workspace capture | `changed_count`, `protected_drop_count`, `metadata_path_count` |
| layerstack publish | `publish_status`, `no_op`, `source_count`, `ignored_count` |
| OCC | `expected_base_version`, `active_version`, conflict reason, redacted class or keyed path/root token when needed |
| remount | `mount_verified`, `staged_switch`, `lowerdir_count_matched`, `probe_content_matched` |
| destroy | `lifetime_s`, `evicted_upperdir_bytes`, `lease_released`, `active_leases_after` |
| cgroup monitor | anomaly/final-summary facts only, such as `sample_kind`, `read_error_count`, cleanup state, and threshold names; periodic CPU, memory, pids, pressure, and disk values belong to metrics |

Do not emit command text, stdin text, command output, raw environment values,
auth tokens, or raw request arguments as trace fields. For command I/O, emit
lengths, offsets, counts, status, and timing only.

## Response And Telemetry Boundary

Protocol responses carry data that the caller needs to continue the workflow.
Telemetry carries operation latency, phase timing, resource trends, cleanup
health, and dashboards. Do not project `sandbox_protocol::Request`,
`sandbox_protocol::Response`, response payloads, `Debug` structs, or raw
`Display` error strings wholesale into spans/events.

| Current payload/stat | Response role | Telemetry replacement |
| --- | --- | --- |
| command `status` and `exit_code` | keep typed; caller needs command state | result fields on command spans/events |
| command `output` and transcript rows | keep typed functional command output | no raw telemetry; offsets and counts only |
| command wall/total time fields | keep only while clients need display fields | span durations and command duration histograms |
| workspace create/destroy phase timing maps | do not add response fields | phase events and histograms |
| remount verification reports | keep typed success summary | booleans, counts, and bounded failure reasons |
| publish/OCC reject payloads | keep typed correctness diagnostics | route counts, reject reason, fingerprint kind, path class/hash |
| public cgroup monitor targets and samples | temporary direct debug/API surface | metrics and final-summary/anomaly events |
| cleanup or runtime error strings | typed/debug diagnostics only | bounded error kind, stage, and counters |

This boundary is what lets response payloads shrink over time. The first
rollout does not change `sandbox_protocol::Response`; later protocol/API cleanup
can remove or narrow response timing and resource fields only after dashboards
and diagnostics read the telemetry backend instead.

## OCC Telemetry Stats

OCC should not be represented as a standalone event object. Split it into the
actual checks and emit those facts as bounded fields on the normal publish
tracing path:

```text
layerstack.expected_base_checked
  expected_base_version
  captured_base_version
  expected_base_root_token
  captured_base_root_token
  result = matched | mismatch

layerstack.source_paths_checked
  checked_count
  result = matched | conflict
  conflict_path_hash
  conflict_path_class
  expected_fingerprint_kind
  actual_fingerprint_kind

layerstack.manifest_commit_checked
  expected_active_version
  found_active_version
  result = committed | manifest_conflict
```

This makes publish failures diagnosable without exposing a new public runtime
trace object API.

## Collection Mode

The production trace sink is OTLP.

The implementation is intentionally phased. Phase 1 is a local JSON tracing
rollout only. The first production OTLP trace rollout happens in Phase 3, after
local subscriber/config behavior and safe-field tests exist. In Phase 3, traces
export to one configured OpenTelemetry Collector endpoint. The collector
handles batching, resource-label normalization, and backend-specific forwarding
to Tempo or another trace backend. Logs and metrics may use the same collector
in later phases, but they are not part of the first OTLP trace rollout unless
explicit log/metric exporters are added and tested.

Telemetry defaults to disabled in local development and tests. Stdout JSON can
exist for explicit foreground debugging and fixtures, but it is a separate
local/test mode, not a fallback from OTLP. File appenders are not a production
transport in this design.

Telemetry delivery must not become a protocol correctness dependency. Exporter
failure uses a bounded exporter queue and may drop telemetry when the collector
is unreachable or the queue is full, but it must not block runtime protocol
responses or silently activate another sink. Invalid telemetry config or a
missing required dynamic `sandbox_id` is a startup error; collector
unreachability after a valid exporter is constructed is fail-open for protocol
behavior.

The canonical local trace backend is Grafana with Tempo. Loki and
Prometheus-compatible metrics belong to later log/metrics phases. Jaeger may be
used as a trace-only smoke target.

## Sandbox Deployment

If `sandbox-daemon` runs inside a sandbox, tracing still works locally because
spans/events are emitted inside the daemon process. Production export depends on
the sandbox being able to reach the collector endpoint:

| Transport | Production Use | Requirement |
| --- | --- | --- |
| OTLP over HTTP or gRPC | yes | sandbox network can reach the configured collector endpoint |
| OTLP to a fixed bridge endpoint | yes, when direct egress is blocked | bridge is mounted/provided at sandbox creation and still speaks OTLP |
| local JSON stream | local/test only | parent/supervisor captures stdout or stderr |
| file | no | not a production transport for this rollout |

`127.0.0.1` inside the sandbox is the sandbox namespace, not necessarily the
host. Do not configure an in-sandbox daemon to export to host `localhost`
unless the collector is actually running in the same namespace.

Preferred production deployment:

```text
sandbox-daemon inside sandbox
  -> OTLP to configured endpoint
  -> collector outside sandbox or fixed bridge endpoint
  -> backend
```

For fully isolated sandboxes with no network egress, use a fixed telemetry
bridge that still receives OTLP. Do not route high-volume telemetry through the
gateway/manager request path.

If daemon TCP request dispatch is enabled for manager/gateway reachability, the
daemon must require a non-empty auth token before binding the TCP listener. TCP
daemon access and OTLP collector access are separate channels and must not share
credentials.

## Multiple Sandboxes

Multiple sandboxes should share one host collector endpoint. Each daemon exports
to the same endpoint and differentiates telemetry using resource attributes:

| Attribute | Value |
| --- | --- |
| `service.name` | `sandbox-daemon` |
| `service.instance.id` | sandbox id |
| `sandbox.id` | sandbox id |
| `container.id` | optional alias when the sandbox backend is container-based |

Do not attach per-request or per-workspace high-cardinality values such as
request IDs, workspace session IDs, command session IDs, raw root hashes, raw
paths, or error strings as resource attributes.

Because the sandbox id is assigned during creation, the manager flow is:

1. Receive `manager.create_sandbox` with a protocol `request_id`.
2. Call `SandboxRuntime::create_sandbox`.
3. Receive `CreateSandboxResult.id`, which is the sandbox id.
4. Store the sandbox record under that id.
5. Install/start the daemon with OTLP endpoint config plus a dynamic identity
   source containing that id.
6. The daemon loads the id at startup and attaches `sandbox.id` and
   `service.instance.id` resource attributes.
7. Query Grafana/Tempo/Loki by `sandbox.id`, `request_id`, or trace id. If the
   collector also derives `container.id`, that label is only an alias.

This avoids per-sandbox host log directories and avoids needing a final sandbox
identity before sandbox creation.

## Trace Lookup UX

Gateway and manager trace support should be lookup UX, not telemetry transport.
Do not stream high-volume daemon spans or logs through the gateway/manager RPC
path.

A future gateway `--trace` or `--verbose-trace` mode may print lookup metadata:

```text
request_id = ...
trace_id = ...
sandbox_id = ...
grafana_url = ...
```

`--trace` should be concise and suitable for normal debugging. `--verbose-trace`
may include span names, selected attributes, and backend query URLs, but it
should still query the backend or use response metadata; it should not proxy the
daemon exporter stream.

Protocol responses may eventually expose trace lookup data as response metadata,
not as operation-specific result fields. That is a later versioned protocol
change because the current response contract returns the operation payload
directly. Phases 1-3 must not depend on this shape. If telemetry is disabled,
or if a later sampling policy decides not to sample a request, `trace_id` is
absent. Protocol results and typed errors remain authoritative.

Illustrative future shape, not a Phase 1-3 response contract:

```json
{
  "result": { "...": "operation payload" },
  "meta": {
    "request_id": "req-1",
    "trace_id": "..."
  }
}
```

## Cross-Process Trace Context

Daemon-internal spans are enough for Phases 1-3. A continuous
daemon-to-runner trace requires explicit context propagation.

Later, pass W3C trace context to `sandbox-daemon ns-runner` using one of:

- `NamespaceRunnerRequest` fields
- environment variables
- a small sidecar/request FD payload

The runner should create child spans under the daemon command span when context
is present and should still emit standalone spans when it is absent.
Only W3C context values such as `traceparent` and `tracestate` may cross this
boundary as telemetry context. Do not propagate command args, cwd, stdin,
environment, auth values, workspace roots, layer paths, cgroup paths, remount
report JSON, or raw request/runner DTOs through telemetry context.

## Configuration Shape

Proposed YAML shape:

```yaml
daemon:
  telemetry:
    enabled: true
    service_name: sandbox-daemon
    level: info
    sink:
      kind: otlp
      endpoint: http://host-otel-collector:4318
      protocol: http
      timeout_ms: 1000
      queue_size: 2048
```

Sandbox identity is runtime state, not static telemetry config. The manager
should pass it to the daemon outside this YAML, for example:

```text
sandbox-daemon serve --sandbox-id <sandbox-id> ...
```

or:

```text
EOS_SANDBOX_ID=<sandbox-id> sandbox-daemon serve ...
```

Config validation rules:

- `telemetry.enabled = false` must deserialize and validate without a sink.
- `telemetry.enabled = true` requires exactly one valid sink.
- `telemetry.level` must be a valid filter level or env-filter expression.
- the initial OTLP trace rollout uses always-on sampling by implementation convention. Do not add
  sampler config until there is more than one real policy; ratio, parent-based,
  and slow/error override sampling policies are deferred until response
  metadata and lookup UX exist.
- exactly one sink is active; no fallback sink list is accepted.
- OTLP endpoints must be explicit; do not infer host networking.
- daemon startup must fail telemetry initialization if manager-started OTLP mode
  has no dynamic `sandbox_id` identity source.
- telemetry config must default to disabled; local JSON streams are allowed only
  as explicit foreground local/test modes. Prefer `stream = stderr` for manual
  debugging unless a fixture explicitly captures stdout.
- local JSON stream mode must be rejected when `sandbox-daemon serve --spawn`
  detaches stdout and stderr, unless a deliberate capture path is added in the
  same change.
- local/test mode must have no hard dependency on an external collector.

## Expected Implementation Impact

This section is a planning estimate for the phased trace rollout. It is not a
license to create extra trace infrastructure.

Phase 1 local JSON scope:

- daemon telemetry config, subscriber setup, and local JSON stream mode
- daemon root request spans with dynamic `sandbox_id`
- inline command runtime spans at existing semantic boundaries
- focused tests proving config validation, local JSON timing, and operation
  instrumentation

Phase 3 adds the first production OTLP exporter after Phase 1 safe-field and
config tests exist.

Deferred scope:

- protocol response metadata for `trace_id`
- gateway `--trace` and `--verbose-trace` lookup UX
- full metrics/dashboard implementation
- custom EphemeralOS UI over backend APIs

Expected file/folder structure:

```text
crates/sandbox-daemon/src/
  telemetry.rs
  serve.rs
  server/
    dispatch.rs
    mod.rs
    runtime.rs

crates/sandbox-config/src/configs/
  daemon.rs

crates/sandbox-runtime/operation/src/
  public/mod.rs
  public/command/service/impls/
    exec_command.rs
    write_command_stdin.rs
    read_command_lines.rs
  public/command/service/finalize.rs
  internal/workspace_session/service/impls/
    create_workspace_session.rs
    capture_session_changes.rs
    destroy_session.rs
    apply_and_finish_remount.rs
    resolve_session.rs
  internal/workspace_remount/service/impls/
    remount_workspace_session.rs
  internal/layerstack/service/impls/
    publish_changes.rs
  internal/cgroup_monitor/        # later, if cgroup stats stop being CLI ops
```

Do not add:

```text
crates/sandbox-runtime-trace/
crates/sandbox-runtime/operation/src/internal/telemetry.rs
```

Expected Rust field and enum additions:

```rust
pub struct DaemonConfig {
    pub server: DaemonServerConfig,
    pub commands: CommandConfig,
    pub cgroup_monitor: CgroupMonitorConfig,
    pub idle_workspace_eviction: IdleWorkspaceEvictionConfig,
    pub telemetry: TelemetryConfig,
}

pub struct TelemetryConfig {
    pub enabled: bool,
    pub service_name: String,
    pub level: String,
    pub sink: Option<TelemetrySink>,
}

pub enum TelemetrySink {
    LocalJson {
        stream: TelemetryOutputStream,
    },
    Otlp {
        endpoint: String,
        protocol: OtlpProtocol,
        timeout_ms: u64,
        queue_size: usize,
    },
}

pub enum TelemetryOutputStream {
    Stdout,
    Stderr,
}

pub enum OtlpProtocol {
    Http,
    Grpc,
}

pub(crate) struct DaemonCliConfig {
    pub(crate) sandbox_id: Option<String>,
    /* existing fields */
}

pub struct ServerConfig {
    pub sandbox_id: Option<String>,
    /* existing fields */
}
```

`DaemonCliConfig` and `ServerConfig` are existing multi-field structs; the
listed `sandbox_id` field is an addition, not a reason to create one-field
wrapper types.

The `SandboxDaemonInstaller` trait does not need a signature change for
sandbox identity because `start_daemon(&SandboxRecord)` already receives the
record id. The concrete daemon starter should pass that id to
`sandbox-daemon serve --sandbox-id <sandbox-id>` or `EOS_SANDBOX_ID`.

Phases 1-3 should not change `sandbox_protocol::Response`. Trace IDs in
protocol responses are a later protocol-envelope change.

Expected changed files for the combined Phases 1-3 trace rollout:

| File | Expected Change | Changed LOC |
| --- | --- | ---: |
| `Cargo.toml` | workspace dependencies for `tracing`, `tracing-subscriber`, and Phase 3 OTel crates | +6 to +10 |
| `config/prd.yml` | `daemon.telemetry` default-disabled config | +6 to +10 |
| `crates/sandbox-config/src/configs/daemon.rs` | telemetry config structs/enums and validation | +90 to +130 |
| `crates/sandbox-config/tests/unit/configs/daemon.rs` | config deserialize/validation tests | +35 to +60 |
| `crates/sandbox-daemon/Cargo.toml` | daemon telemetry dependencies | +6 to +10 |
| `crates/sandbox-daemon/src/telemetry.rs` | subscriber setup, local JSON stream, Phase 3 OTLP setup, bounded exporter behavior, tracked shutdown flush, resource attributes | +220 to +340 |
| `crates/sandbox-daemon/src/serve.rs` | parse `--sandbox-id`, init telemetry, reject local JSON streams with detached spawn, pass identity to server config | +60 to +110 |
| `crates/sandbox-daemon/src/server/runtime.rs` | `sandbox_id` on `ServerConfig` | +8 to +20 |
| `crates/sandbox-daemon/src/server/dispatch.rs` | `daemon.request` root span and request fields | +30 to +60 |
| `crates/sandbox-daemon/src/server/mod.rs` | expose telemetry module if needed | +1 to +3 |
| `crates/sandbox-daemon/tests/unit/telemetry.rs` | telemetry config/subscriber/exporter shutdown tests | +100 to +170 |
| `crates/sandbox-daemon/tests/unit/serve.rs` | CLI/env `sandbox_id`, local JSON stream spawn rejection, and telemetry parsing tests | +40 to +80 |
| `crates/sandbox-daemon/tests/unit/dispatch.rs` | root span/request safe-field assertions | +40 to +70 |
| `crates/sandbox-runtime/operation/Cargo.toml` | `tracing` dependency | +1 |
| `crates/sandbox-runtime/operation/src/public/mod.rs` | runtime dispatch span | +15 to +30 |
| `crates/sandbox-runtime/operation/src/public/command/service/impls/exec_command.rs` | command start/yield/finalization spans/events | +35 to +65 |
| `crates/sandbox-runtime/operation/src/public/command/service/impls/write_command_stdin.rs` | stdin write span and result fields | +15 to +30 |
| `crates/sandbox-runtime/operation/src/public/command/service/impls/read_command_lines.rs` | read span and output window fields | +15 to +30 |
| `crates/sandbox-runtime/operation/src/public/command/service/finalize.rs` | command finalization events and result fields | +25 to +50 |
| `crates/sandbox-runtime/operation/src/internal/workspace_session/service/impls/*.rs` | create/capture/destroy/remount session spans/events | +60 to +100 total |
| `crates/sandbox-runtime/operation/src/internal/workspace_remount/service/impls/remount_workspace_session.rs` | remount orchestration span/events | +20 to +40 |
| `crates/sandbox-runtime/operation/src/internal/layerstack/service/impls/publish_changes.rs` | publish/OCC telemetry stats | +25 to +50 |
| `crates/sandbox-runtime/operation/src/internal/cgroup_monitor/*` | cgroup anomaly/final-summary trace events or metrics-only adapters if cgroup stats are removed from CLI operation specs | +35 to +80 total |
| `crates/sandbox-runtime/operation/tests/*` | focused trace assertions, safe-field assertions, no periodic cgroup trace events | +220 to +380 total |

Expected Phase 1 total: about 650 to 1,020 changed LOC. Expected Phases 1-3
combined production trace rollout: about 1,300 to 2,300 changed LOC.

If protocol response metadata, gateway trace lookup, and metrics dashboards are
implemented in the same batch, expect another 500 to 900 net LOC and additional
changes under `sandbox-protocol`, `sandbox-gateway`, and `sandbox-manager`.
Keep those as separate phases unless there is a concrete release need to batch
them.

## Implementation Plan

Detailed implementation specs with per-phase file/folder changes,
struct/class-field changes, LOC estimates, and acceptance checklists live in
[phases/README.md](phases/README.md).

### Phase 1: Local JSON tracing

- Add `tracing` to runtime crates that emit spans/events.
- Add `tracing-subscriber` to `sandbox-daemon`.
- Initialize a subscriber in `sandbox-daemon serve`.
- Reject local JSON stream telemetry when `serve --spawn` would detach
  stdout/stderr.
- Add `daemon.request` root span around `dispatch_request`.
- Add spans to runtime operation dispatch and command operations.
- If a generic dispatch span is kept, name it `runtime.dispatch`; operation
  spans keep the stable `runtime.exec_command`, `runtime.write_command_stdin`,
  and `runtime.read_command_lines` names.
- Make span-close timing visible in local JSON output so operation wall-clock
  duration can be inspected without a backend.
- Assert root spans record explicit safe fields and do not record raw request
  args, response payloads, command text, stdin, output, environment values, or
  auth tokens, raw paths, raw PIDs, raw root hashes, or raw DTO/error strings.
- Keep runtime instrumentation inline in existing modules; do not add
  `crates/sandbox-runtime/operation/src/internal/telemetry.rs`.
- Enable `FmtSpan::CLOSE` or equivalent span-close timing.
- Verify existing tests and add focused JSON formatting/config tests.

### Phase 2: Runtime semantic spans

- Add spans/events for workspace create/destroy/capture/remount at the existing
  service methods.
- Convert existing internal workspace create phase timers into trace events
  while keeping `WorkspaceHandle` behavior unchanged. Preserve explicit
  `Instant` phase timers for typed reports where they exist.
- Emit remount verification diagnostic fields as events while preserving the
  simplified `RemountOverlayResult` correctness surface. Do not project raw
  namespace-process remount report JSON.
- Emit layerstack publish route/OCC/publish result events from operation-level
  wrappers.
- Emit cgroup monitor trace events only for internal anomalies and final
  summaries. Do not add spans for `inspect_cgroup_monitor` or
  `read_cgroup_monitor_samples`.

### Phase 3: OTLP export

- Add optional OpenTelemetry dependencies.
- Select exact compatible OTel crate versions and protocol features in one
  Cargo change.
- Add config schema for telemetry sink and OTLP endpoint.
- Export traces to collector through one OTLP path.
- Keep local JSON streams only as explicit foreground local/test modes.
- Add validation that production daemon config has one sink and no fallback.
- Add bounded exporter queue/drop policy and define collector-unreachable
  behavior as fail-open for protocol responses after config validation.
- Track or drain spawned request/connection tasks before flushing or shutting
  down the telemetry provider so terminal spans/events are not lost on normal
  daemon shutdown.
- Add tests proving daemon startup succeeds when OTLP is disabled.

### Phase 4a: Metrics and dashboards

- Add latency histograms for runtime operations and workspace phases. These
  should come from span durations or direct histogram recording, not from
  subtracting unrelated event timestamps or reading command response timing
  fields.
- Add counters for publish rejection reasons, remount failures, command
  cancellations, and cgroup monitor read errors.
- Before dashboards depend on command final cgroup samples, regression-test the
  deterministic final-sample-before-cleanup ordering so a post-cleanup periodic
  sample cannot affect final CPU delta/percent enrichment.
- Export cgroup monitor samples as metrics first. Periodic CPU, memory, pids,
  pressure, and disk samples should not be emitted as per-sample trace events.
- Record cgroup metrics through a daemon-owned metrics recorder or narrow
  injected interface, not by giving runtime crates exporter/subscriber
  ownership and not by polling public cgroup read operations.
- Emit trace events for cgroup anomalies and final summaries only, such as read
  failures, cleanup failures, pressure threshold crossings, and command final
  samples.
- Keep existing `inspect_cgroup_monitor` and `read_cgroup_monitor_samples`
  operations temporarily, but dashboards must not depend on them.
- Build dashboards for command latency, publish conflict rate, remount health,
  and cgroup resource trends.

### Phase 4b: Cgroup telemetry cutover

- Start only after Phase 4a dashboards read cgroup stats from telemetry metrics.
- Move `crates/sandbox-runtime/operation/src/public/cgroup_monitor` to an
  internal service, or collapse the remaining code into internal workspace and
  command telemetry adapters.
- Remove `inspect_cgroup_monitor` and `read_cgroup_monitor_samples` from runtime
  `cli_operation_specs`, CLI operation families, operation entries,
  daemon-described runtime catalog output, and gateway runtime help/mappings.
- Keep `CgroupMonitorSample`, final samples, cleanup state, and retained
  internal samples as metrics sources.
- Do not leave hidden compatibility aliases for the old cgroup monitor
  operation names.

### Phase 5: Runner context propagation

- Pass trace context from daemon to `ns-runner`.
- Cover both command launches and workspace overlay/remount setns-runner
  launches.
- Add runner spans for setns, cgroup join, overlay mount/remount, and command
  execution.
- Preserve compatibility when trace context is absent, invalid, or omitted from
  older `NamespaceRunnerRequest` payloads.

## Verification

Focused checks for each implementation batch:

```sh
cargo fmt --check
cargo check -p sandbox-daemon -p sandbox-runtime --all-targets
cargo test -p sandbox-daemon -p sandbox-runtime
git diff --check -- docs/trace crates/sandbox-daemon crates/sandbox-runtime
```

When workspace/namespace/layerstack instrumentation is touched, add:

```sh
cargo test -p sandbox-runtime-workspace
cargo test -p sandbox-runtime-layerstack
cargo test -p sandbox-runtime-namespace-process
```

Trace assertions should verify:

- root span includes `request_id` and operation name
- daemon root spans include `sandbox_id` after sandbox creation
- child spans include workspace/command IDs when available
- span-close output or exported spans include wall-clock duration for operation
  boundaries
- explicit phase events include `duration_ms` when they represent timed phases
- failures emit error fields and do not suppress normal protocol errors
- OTLP mode does not open a secondary stdout/file/manager-RPC fallback sink
- OTLP shutdown flush is called on normal daemon shutdown
- exporter failure uses bounded drop/queue behavior and does not block protocol
  responses
- telemetry disabled path has no external network dependency
- explicit local JSON stream mode works only as foreground local/test mode and is
  rejected with detached `--spawn`
- trace assertions do not contain raw request args, command text, stdin,
  command output, environment values, auth tokens, raw host paths, raw
  workspace roots, raw cgroup paths, raw layer paths, raw upper/work dirs,
  transcript/artifact paths, raw PIDs, raw root hashes, raw DTO `Debug`, raw
  response payloads, or raw `Display` error strings
- sentinel tests inject command text, stdin, stdout/stderr, env/auth-like
  values, and paths, then assert those values never appear in telemetry
- protocol tests prove first trace phases do not add a `result` wrapper, `meta`
  object, `trace_id`, or other telemetry field to `sandbox_protocol::Response`
- gateway trace mode does not stream telemetry through gateway/manager RPC
- cgroup periodic samples are exported as metrics, not per-sample trace events
- cgroup trace events are limited to anomalies and final summaries
- metric labels are allowlisted and exclude request IDs, workspace session IDs,
  command session IDs, PIDs, path-derived IDs, raw paths, raw root hashes, and
  free-form error strings
- command transcript behavior is unchanged

## Resolved Decisions

| Decision | Resolution |
| --- | --- |
| Local development default | disabled; local JSON stream is explicit local/test mode only; stderr is the preferred manual-debug stream |
| Gateway trace mode | yes, as lookup UX only; never telemetry transport |
| Trace IDs in protocol responses | later only, as a versioned protocol-envelope change |
| Cgroup monitor samples | metrics primary; trace events only for anomalies/final summaries |
| Response simplification | telemetry owns time/resource/dashboard stats; responses keep workflow data only until a later API cleanup |
| Canonical production OTLP trace backend | Grafana + Tempo for traces; Loki and Prometheus-compatible metrics are later phases; Jaeger is optional trace-only smoke target |
| Time measurement | span duration for operation latency, event timestamps for ordering, explicit `Instant` timers for typed phase reports |
