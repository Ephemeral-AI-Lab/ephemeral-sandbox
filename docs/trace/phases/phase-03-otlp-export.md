# Phase 3: OTLP Export

## Goal

Add production trace export through one configured OTLP path. This phase keeps
telemetry delivery out of protocol correctness, adds bounded exporter failure
behavior, and flushes terminal spans on daemon shutdown.

## Scope

- Add exact OpenTelemetry crate versions and feature flags.
- Extend daemon telemetry config with OTLP settings.
- Export traces to one configured Collector endpoint.
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

crates/sandbox-runtime/config/src/configs/
  daemon.rs

crates/sandbox-runtime/config/tests/unit/configs/
  daemon.rs
```

Do not add file appenders or manager/gateway RPC telemetry transport.

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

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| OTel dependencies and feature selection | 8 to 20 |
| Config structs, validation, tests | 120 to 200 |
| OTLP exporter setup | 140 to 240 |
| Shutdown task tracking and flush integration | 90 to 160 |
| Exporter failure/drop/resource tests | 110 to 190 |
| Docs/config examples | 32 to 80 |
| Total | 520 to 910 |

## Acceptance Criteria

- [ ] OTel dependencies use exact compatible versions and documented feature
      flags in `Cargo.toml`; no placeholder versions, wildcard `0.x`
      declarations, or mixed-generation OTel crates remain.
- [ ] OTLP config accepts exactly one active sink.
- [ ] File sink and fallback sink lists are rejected.
- [ ] OTLP mode requires dynamic `sandbox_id` for manager-started daemons.
- [ ] OTLP resource attributes include `service.name`, `service.instance.id`,
      and `sandbox.id`, and exclude raw paths, root hashes, request IDs,
      command IDs, workspace session IDs, cgroup paths, and error strings.
- [ ] Invalid telemetry config fails daemon startup.
- [ ] Collector unreachable after valid exporter construction does not alter
      runtime protocol responses.
- [ ] Exporter timeout, queue/drop behavior, and flush error behavior are
      bounded and covered by tests.
- [ ] Normal daemon shutdown drains or cancels tracked request/connection tasks
      before flushing or shutting down the telemetry provider.
- [ ] Stdout/stderr JSON remain foreground local/test only.
- [ ] Local JSON stream mode is still rejected under detached `serve --spawn`.
- [ ] No `sandbox_protocol::Response` metadata or envelope change is introduced.
- [ ] `cargo test -p sandbox-daemon -p sandbox-runtime-config -p sandbox-protocol` passes.
