# Phase 3: OTLP Export

## Goal

Add production trace export through one configured OTLP path. This phase keeps
telemetry delivery out of protocol correctness, adds bounded exporter failure
behavior, and flushes terminal spans on daemon shutdown.

## Scope

- Add exact OpenTelemetry crate versions and feature flags.
- Extend daemon telemetry config with OTLP settings.
- Export traces to one configured Collector endpoint.
- Add validation environment config for OpenTelemetry Collector, Tempo, and
  Grafana. This environment validates trace export and trace-event visibility
  only.
- Fail startup for invalid telemetry config or missing dynamic identity.
- Fail open for protocol behavior after a valid exporter is constructed.
- Track or drain in-flight connection/request tasks on normal daemon shutdown,
  then flush/shut down the telemetry provider. A flush that runs while spawned
  request tasks can still close spans is not sufficient.

## File And Folder Structure Changes

```text
Cargo.toml
  [workspace.dependencies]
    # Select exact compatible OpenTelemetry crate versions in the implementation
    # Cargo change. Do not leave placeholders, wildcards, or mixed-generation
    # OTel crates in Cargo.toml.
    tracing-opentelemetry
    opentelemetry
    opentelemetry_sdk
    opentelemetry-otlp with exactly one configured transport feature

crates/sandbox-daemon/
  Cargo.toml
  src/
    telemetry.rs
    serve.rs
  tests/
    unit/
      telemetry.rs
      serve.rs

crates/sandbox-config/src/configs/
  daemon.rs

crates/sandbox-config/tests/unit/configs/
  daemon.rs

observability/
  phase-03-traces/
    docker-compose.yml        # collector, tempo, grafana; no loki
    otel-collector.yaml       # trace pipeline only
    tempo.yaml
    grafana/
      provisioning/
        datasources/
          tempo.yaml
```

Phase 3 creates `observability/phase-03-traces/` with the trace-only services
and provisioning needed for OTLP trace validation. Later phases must not add
Loki, log exporters, or trace-to-logs configuration to this Phase 3 stack.

Do not add file appenders, Loki, log exporters, or manager/gateway RPC
telemetry transport.

## Struct/Class And Field Changes

```rust
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

pub(crate) struct TelemetryGuard {
    /* owns provider/subscriber guard and flush-on-drop or explicit shutdown */
}
```

No `sandbox_protocol::Response` metadata is added in this phase.

OTLP resource attributes must include `service.name = sandbox-daemon`,
`service.instance.id = sandbox_id`, and `sandbox.id = sandbox_id`. They must not
include raw paths, raw root hashes, request IDs, command IDs, workspace session
IDs, cgroup paths, or other per-request/per-workspace high-cardinality values.

## Exporter Rules

- Exactly one active sink is accepted.
- Production trace sink is OTLP only.
- Phase 3 exports trace data only. Spans and trace events go to Tempo through
  the collector. They are not log records and do not require Loki.
- Stdout/stderr JSON remain local/test foreground modes only. Prefer stderr for
  manual debugging unless the test fixture explicitly captures stdout.
- File sink is not accepted.
- Collector endpoint must be explicit.
- `127.0.0.1` must not be inferred as a host collector from inside a sandbox.
- Collector unreachable after exporter construction may drop/queue telemetry but
  must not block protocol responses.
- Exporter queue size must be bounded.
- Exporter timeout, queue size, and drop behavior must be explicit and tested.
  Queue-full and collector-unreachable conditions may drop telemetry but must
  not block protocol responses after exporter construction succeeds.
- Normal daemon shutdown must stop accepting new requests, drain or cancel
  tracked request/connection tasks to a defined boundary, then flush or shut
  down the provider.
- Do not add sampler config until there is at least one real policy beyond the
  initial OTLP trace rollout. Initial OTLP trace sampling is always-on by
  implementation convention, not a one-variant config enum.
- The required validation environment is OpenTelemetry Collector plus Tempo
  plus Grafana. Jaeger may be added as an optional trace-only smoke target, but
  it does not replace Tempo/Grafana validation.
- Do not configure Grafana trace-to-logs or Loki in this phase.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| OTel dependencies and feature selection | 8 to 20 |
| Config structs, validation, tests | 120 to 200 |
| OTLP exporter setup | 140 to 240 |
| Shutdown task tracking and flush integration | 90 to 160 |
| Exporter failure/drop/resource tests | 110 to 190 |
| Trace validation environment/provisioning | 80 to 150 |
| Docs/config examples | 32 to 80 |
| Total | 600 to 1,060 |

## Acceptance Criteria

- [x] OTel dependencies use exact compatible versions and documented feature
      flags in `Cargo.toml`; no placeholder versions, wildcard `0.x`
      declarations, or mixed-generation OTel crates remain.
- [x] OTLP config accepts exactly one active sink.
- [x] File sink and fallback sink lists are rejected.
- [x] OTLP mode requires dynamic `sandbox_id` for manager-started daemons.
- [x] OTLP resource attributes include `service.name`, `service.instance.id`,
      and `sandbox.id`, and exclude raw paths, root hashes, request IDs,
      command IDs, workspace session IDs, cgroup paths, and error strings.
- [x] Validation environment includes OpenTelemetry Collector, Tempo, and
      Grafana with a Tempo data source under `observability/phase-03-traces/`.
- [x] The Phase 3 validation environment does not include Loki, log exporters,
      or Grafana trace-to-logs configuration.
- [x] Invalid telemetry config fails daemon startup.
- [x] Collector unreachable after valid exporter construction does not alter
      runtime protocol responses.
- [x] Exporter timeout, queue/drop behavior, and flush error behavior are
      bounded and covered by tests.
- [x] Normal daemon shutdown drains or cancels tracked request/connection tasks
      before flushing or shutting down the telemetry provider.
- [x] Stdout/stderr JSON remain foreground local/test only.
- [x] Local JSON stream mode is still rejected under detached `serve --spawn`.
- [x] No `sandbox_protocol::Response` metadata or envelope change is introduced.
- [x] `cargo test -p sandbox-daemon -p sandbox-config -p sandbox-protocol` passes.
