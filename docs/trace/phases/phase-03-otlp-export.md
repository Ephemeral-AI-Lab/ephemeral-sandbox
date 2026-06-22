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
- Flush/shut down the telemetry provider after `server.serve()` returns.

## File And Folder Structure Changes

```text
Cargo.toml
  [workspace.dependencies]
    tracing-opentelemetry = "<exact version>"
    opentelemetry = "<exact version>"
    opentelemetry_sdk = "<exact version>"
    opentelemetry-otlp = { version = "<exact version>", features = ["<http-or-grpc-feature>"] }

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
    pub sink: TelemetrySink,
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
- Normal daemon shutdown must flush or shut down the provider.
- Do not add sampler config until there is at least one real policy beyond the
  default first-rollout behavior. First rollout sampling is always-on by
  implementation convention, not a one-variant config enum.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| OTel dependencies and feature selection | 8 to 20 |
| Config structs, validation, tests | 120 to 200 |
| OTLP exporter setup | 140 to 240 |
| Shutdown guard/flush integration | 50 to 90 |
| Exporter failure tests | 70 to 130 |
| Docs/config examples | 32 to 80 |
| Total | 420 to 760 |

## Acceptance Criteria

- [ ] OTel dependencies use exact compatible versions and documented feature
      flags.
- [ ] OTLP config accepts exactly one active sink.
- [ ] File sink and fallback sink lists are rejected.
- [ ] OTLP mode requires dynamic `sandbox_id` for manager-started daemons.
- [ ] Invalid telemetry config fails daemon startup.
- [ ] Collector unreachable after valid exporter construction does not alter
      runtime protocol responses.
- [ ] Exporter queue/drop behavior is bounded and covered by tests.
- [ ] Normal daemon shutdown flushes or shuts down the telemetry provider.
- [ ] Stdout/stderr JSON remain foreground local/test only.
- [ ] `cargo test -p sandbox-daemon -p sandbox-runtime-config` passes.
