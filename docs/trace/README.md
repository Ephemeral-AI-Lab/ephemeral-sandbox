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

## Initial Dependencies

Add these workspace dependencies when implementation starts:

```toml
tracing = { version = "0.1", features = ["attributes"] }
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
```

Runtime crates should depend only on `tracing` unless they have a concrete
reason to own subscriber behavior. `sandbox-daemon` owns `tracing-subscriber`
setup.

Add these only when OTLP export is implemented:

```toml
tracing-opentelemetry = "0.x"
opentelemetry = "0.x"
opentelemetry_sdk = "0.x"
opentelemetry-otlp = "0.x"
```

Exact OpenTelemetry crate versions should be selected together because the Rust
OTel crates move in lockstep.

## Code Ownership

Keep the tracing ownership narrow:

| Area | Owns |
| --- | --- |
| `sandbox-daemon` | telemetry initialization, subscriber setup, dynamic `container_id`, OTLP exporter |
| `sandbox-runtime` operation crate | inline spans/events in existing command, workspace, remount, layerstack wrappers |
| `sandbox-runtime-workspace` | inline spans/events at stable workspace service boundaries when needed |
| `sandbox-runtime-layerstack` | structured results/errors; no custom trace-event objects |
| `sandbox-runtime-overlay` | no tracing by default unless a low-level mount syscall cannot be diagnosed from callers |
| `sandbox-runtime-config` | typed telemetry config only |

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
          workspace.capture_changes
            overlay.capture_upperdir
          layerstack.publish_changes
            layerstack.publish.plan
            layerstack.occ.validate_source_paths
            layerstack.publish.commit_manifest
          workspace.remount
        workspace.destroy_session

      runtime.write_command_stdin
      runtime.read_command_lines
      runtime.inspect_cgroup_monitor
      runtime.read_cgroup_monitor_samples
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
| `container_id` | daemon runtime identity loaded at daemon startup |
| `scope_sandbox_id` | request scope value when the protocol carries one |

`container_id` is the canonical runtime identity. It is not pre-known for
`manager.create_sandbox`; the runtime creates the container and returns the
resulting id. Traces emitted before that point must use `request_id` or an
explicit `creation_correlation_id`, then record an event when the container id is
assigned:

```text
manager.create_sandbox
  request_id = ...
  creation_correlation_id = ...
  event sandbox.container_created { container_id }
```

After creation succeeds, the manager starts the daemon with a dynamic identity
source, for example an environment variable, daemon argument, or generated
identity file. The static telemetry YAML should not contain `container_id` or a
separate `sandbox_id`. At startup, the daemon reads the container id once and
attaches it to all root spans and OpenTelemetry resource attributes. If a
`sandbox.id` label is useful for Grafana queries, derive it from `container_id`;
do not configure it as a second source of truth.

Runtime child spans should add the IDs they own:

| Field | Owner |
| --- | --- |
| `workspace_session_id` | workspace session service |
| `command_session_id` | command service |
| `lease_id` | workspace/layerstack |
| `manifest_version` | workspace/layerstack |
| `root_hash` | workspace/layerstack |
| `cgroup_path_present` | workspace/command |

Paths should be recorded carefully. Prefer booleans, counts, and stable IDs over
full paths in high-volume spans. Full paths are acceptable for debug-level events
when they are needed to diagnose a failure.

## Span Names

Use dot-separated stable names:

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
layerstack.publish.plan
layerstack.occ.validate_source_paths
layerstack.publish.commit_manifest
command.spawn
command.wait_initial_yield
command.finalize
cgroup_monitor.inspect
cgroup_monitor.read_samples
```

Avoid names that mirror temporary private helper files unless the helper is a
stable diagnostic boundary.

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
cgroup_sample_recorded
cleanup_finished
```

## Result Fields To Emit

Emit these fields as span fields or events while keeping them in typed results
where they already exist:

| Area | Fields |
| --- | --- |
| command | `status`, `exit_code`, `cancelled`, `timed_out` |
| workspace create | phase name, phase duration, profile |
| workspace capture | `changed_count`, `protected_drop_count`, `metadata_path_count` |
| layerstack publish | `publish_status`, `no_op`, `source_count`, `ignored_count` |
| OCC | `expected_base_version`, `active_version`, conflict reason |
| remount | `mount_verified`, `staged_switch`, `lowerdir_count_matched`, `probe_content_matched` |
| destroy | `lifetime_s`, `evicted_upperdir_bytes`, `lease_released`, `active_leases_after` |
| cgroup monitor | CPU, memory, pids, pressure, disk sample availability and read errors |

## OCC Event Model

OCC should not be represented as one vague event. Split it into the actual
checks:

```text
layerstack.expected_base_checked
  expected_base_version
  expected_base_root_hash
  captured_base_version
  result = matched | mismatch

layerstack.source_paths_checked
  checked_count
  result = matched | conflict
  conflict_path
  expected_fingerprint_kind
  actual_fingerprint_kind

layerstack.manifest_commit_checked
  expected_active_version
  found_active_version
  result = committed | manifest_conflict
```

This makes publish failures diagnosable without exposing a new public
`OccTraceEvent` API.

## Collection Mode

The production sink is OTLP.

The daemon exports traces/logs/metrics to one configured OpenTelemetry Collector
endpoint. The collector handles batching, resource-label normalization, and
backend-specific forwarding to Tempo/Loki/Prometheus/Grafana.

Telemetry defaults to disabled in local development and tests. Stdout JSON can
exist for explicit foreground debugging and fixtures, but it is a separate mode,
not a fallback from OTLP. File appenders should not be part of the production
multi-sandbox design unless the product explicitly chooses file collection as
the only active transport for that deployment.

Telemetry delivery must not become a protocol correctness dependency. Exporter
failure may drop or queue telemetry according to the configured exporter, but it
must not silently activate another sink.

The canonical local backend is Grafana with Tempo for traces, Loki for logs, and
Prometheus-compatible metrics. Jaeger may be used as a trace-only smoke target,
but it is not the canonical development stack because it does not cover logs and
metrics.

## Sandbox Deployment

If `sandbox-daemon` runs inside a sandbox, tracing still works locally because
spans/events are emitted inside the daemon process. Production export depends on
the sandbox being able to reach the collector endpoint:

| Sink | Production Use | Requirement |
| --- | --- | --- |
| OTLP over TCP/HTTP | preferred | sandbox network can reach host collector endpoint |
| Unix socket or bridge endpoint | use when network egress is blocked | fixed bridge is mounted/provided at container creation |
| stdout/stderr JSON | local/test only | parent/supervisor captures process output |
| file | not preferred | chosen as sole transport and provisioned before daemon start |

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
bridge as the chosen transport. Do not route high-volume telemetry through the
gateway/manager request path.

## Multiple Sandboxes

Multiple sandboxes should share one host collector endpoint. Each daemon exports
to the same endpoint and differentiates telemetry using resource attributes:

| Attribute | Value |
| --- | --- |
| `service.name` | `sandbox-daemon` |
| `service.instance.id` | container id |
| `container.id` | container id |
| `sandbox.id` | optional alias derived from container id |
| `workspace.root_hash` | optional hash, not raw path |

Because the container id is assigned during creation, the manager flow is:

1. Receive `manager.create_sandbox` with a protocol `request_id`.
2. Call `SandboxRuntime::create_sandbox`.
3. Receive `CreateSandboxResult.id`, which is the container id.
4. Store the sandbox record under that id.
5. Install/start the daemon with OTLP endpoint config plus a dynamic identity
   source containing that id.
6. The daemon loads the id at startup and attaches `container.id` and
   `service.instance.id` resource attributes.
7. Query Grafana/Tempo/Loki by `container.id`, `request_id`, or trace id. If the
   collector also derives `sandbox.id`, that label is only an alias.

This avoids per-sandbox host log directories and avoids needing a final
container identity before container creation.

## Trace Lookup UX

Gateway and manager trace support should be lookup UX, not telemetry transport.
Do not stream high-volume daemon spans or logs through the gateway/manager RPC
path.

A future gateway `--trace` or `--verbose-trace` mode may print lookup metadata:

```text
request_id = ...
trace_id = ...
container_id = ...
grafana_url = ...
```

`--trace` should be concise and suitable for normal debugging. `--verbose-trace`
may include span names, selected attributes, and backend query URLs, but it
should still query the backend or use response metadata; it should not proxy the
daemon exporter stream.

Protocol responses should eventually expose trace lookup data as optional
response metadata, not as operation-specific result fields. If telemetry is
disabled or no trace was sampled, `trace_id` is absent. Protocol results and
typed errors remain authoritative.

Example future shape:

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

Daemon-internal spans are enough for the first rollout. A continuous
daemon-to-runner trace requires explicit context propagation.

Later, pass W3C trace context to `sandbox-daemon ns-runner` using one of:

- `NamespaceCommandRequest` fields
- environment variables
- a small sidecar/request FD payload

The runner should create child spans under the daemon command span when context
is present and should still emit standalone spans when it is absent.

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
    otlp:
      endpoint: http://host-otel-collector:4318
      protocol: http
      timeout_ms: 1000
```

Container identity is runtime state, not static telemetry config. The manager
should pass it to the daemon outside this YAML, for example:

```text
sandbox-daemon serve --container-id <container-id> ...
```

or:

```text
EOS_CONTAINER_ID=<container-id> sandbox-daemon serve ...
```

Config validation rules:

- `telemetry.level` must be a valid filter level or env-filter expression.
- exactly one sink is active; no fallback sink list is accepted.
- OTLP endpoints must be explicit; do not infer host networking.
- daemon startup must fail telemetry initialization if manager-started OTLP mode
  has no dynamic `container_id` identity source.
- telemetry config must default to disabled; stdout JSON is allowed only as an
  explicit local/test mode.
- local/test mode must have no hard dependency on an external collector.

## Implementation Plan

### Phase 1: Local JSON tracing

- Add `tracing` to runtime crates that emit spans/events.
- Add `tracing-subscriber` to `sandbox-daemon`.
- Initialize a subscriber in `sandbox-daemon serve`.
- Add `daemon.request` root span around `dispatch_request`.
- Add spans to runtime operation dispatch and command operations.
- Keep runtime instrumentation inline in existing modules; do not add
  `crates/sandbox-runtime/operation/src/internal/telemetry.rs`.
- Enable `FmtSpan::CLOSE` or equivalent span-close timing.
- Verify existing tests and add focused JSON formatting/config tests.

### Phase 2: Runtime semantic spans

- Add spans/events for workspace create/destroy/capture/remount at the existing
  service methods.
- Convert existing create `phases_ms` records into trace events while keeping
  any typed result behavior unchanged.
- Emit remount verification report fields as events while preserving
  `RemountOverlayReport`.
- Emit layerstack publish route/OCC/publish result events from operation-level
  wrappers.

### Phase 3: OTLP export

- Add optional OpenTelemetry dependencies.
- Add config schema for telemetry sink and OTLP endpoint.
- Export traces to collector.
- Keep stdout JSON only as an explicit local/test mode.
- Add validation that production daemon config has one sink and no fallback.
- Add tests proving daemon startup succeeds when OTLP is disabled.

### Phase 4: Metrics and dashboards

- Add latency histograms for runtime operations and workspace phases.
- Add counters for publish rejection reasons, remount failures, command
  cancellations, and cgroup monitor read errors.
- Export cgroup monitor samples as metrics first. Periodic CPU, memory, pids,
  pressure, and disk samples should not be emitted as per-sample trace events.
- Emit trace events for cgroup anomalies and final summaries only, such as read
  failures, cleanup failures, pressure threshold crossings, and command final
  samples.
- Build dashboards for command latency, publish conflict rate, remount health,
  and cgroup resource trends.

### Phase 5: Runner context propagation

- Pass trace context from daemon to `ns-runner`.
- Add runner spans for setns, cgroup join, overlay mount/remount, and command
  execution.
- Preserve compatibility when trace context is absent.

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
- daemon root spans include `container_id` after container creation
- child spans include workspace/command IDs when available
- failures emit error fields and do not suppress normal protocol errors
- OTLP mode does not open a secondary stdout/file/manager-RPC fallback sink
- telemetry disabled path has no external network dependency
- explicit stdout JSON mode works only as local/test mode
- gateway trace mode does not stream telemetry through gateway/manager RPC
- cgroup periodic samples are exported as metrics, not per-sample trace events
- command transcript behavior is unchanged

## Resolved Decisions

| Decision | Resolution |
| --- | --- |
| Local development default | disabled; stdout JSON is explicit local/test mode only |
| Gateway trace mode | yes, as lookup UX only; never telemetry transport |
| Trace IDs in protocol responses | yes, later as optional response metadata |
| Cgroup monitor samples | metrics primary; trace events only for anomalies/final summaries |
| Canonical development backend | Grafana + Tempo + Loki + Prometheus-compatible metrics; Jaeger is optional trace-only smoke target |
