# Phase 4a: Metrics And Dashboards

## Goal

Add metrics and dashboards after trace export is stable. Cgroup monitor periodic
samples become metrics first; traces receive only anomaly and final-summary
events. This phase proves telemetry can carry cgroup stats, but it does not
remove the existing CLI/catalog-facing cgroup monitor operations yet.

## Scope

- Add latency histograms for runtime operations and workspace phases.
- Add counters for publish rejections, remount failures, command cancellations,
  and cgroup read errors.
- Preserve and regression-test command final sample and cleanup ordering before
  using command final cgroup samples as dashboard inputs. A post-cleanup
  periodic sample must not be able to become the retained previous sample for
  final CPU delta/percent enrichment.
- Export periodic cgroup CPU, memory, pids, pressure, and disk samples as
  metrics.
- Add dashboard definitions for command latency, publish conflict rate, remount
  health, and cgroup resource trends.
- Keep `inspect_cgroup_monitor` and `read_cgroup_monitor_samples` temporarily
  until Phase 4b proves they are no longer needed as CLI/debug surfaces.

## File And Folder Structure Changes

```text
Cargo.toml
  [workspace.dependencies]
    opentelemetry metrics features, if not already enabled

crates/sandbox-daemon/src/
  telemetry.rs
  telemetry/
    metrics.rs              # daemon-owned recorder/exporter setup

crates/sandbox-runtime/config/src/configs/
  daemon.rs

crates/sandbox-runtime/command/src/
  process.rs
  cgroup.rs

crates/sandbox-runtime/workspace/src/namespace/
  cgroup_monitor.rs

crates/sandbox-runtime/operation/src/
  internal/workspace_session/service/impls/
  internal/workspace_remount/service/impls/
  internal/layerstack/service/impls/

docs/trace/dashboards/
  command-latency.json
  publish-conflicts.json
  remount-health.json
  cgroup-resources.json
```

If `telemetry/metrics.rs` is added, `telemetry.rs` remains daemon-owned. Runtime
crates still must not own exporter setup. Runtime call sites may use only a
narrow metrics recorder interface injected by daemon/runtime construction. That
interface may expose bounded domain methods such as
`record_runtime_latency(...)`, `record_workspace_phase(...)`, and
`record_cgroup_sample(...)`; it must not expose OTLP SDK/exporter types or allow
arbitrary label maps.

## Struct/Class And Field Changes

```rust
pub struct TelemetryConfig {
    pub enabled: bool,
    pub service_name: String,
    pub level: String,
    pub sink: TelemetrySink,
    pub metrics: Option<TelemetryMetricsConfig>,
}

pub struct TelemetryMetricsConfig {
    pub enabled: bool,
    pub export_interval_ms: u64,
    pub cgroup_samples_enabled: bool,
}
```

No cgroup monitor API response fields are required to change in Phase 4a.
Existing `inspect_cgroup_monitor` and `read_cgroup_monitor_samples` operations
remain available until the Phase 4b cutover. `CgroupMonitorSample` becomes the
typed metrics source while still supporting the existing direct read API. The
direct read API is not the canonical telemetry interface and dashboards must not
depend on its response shape. Metrics must be emitted at sample creation,
finalization, or cleanup boundaries; do not poll the public read operations to
feed metrics.

## Metric Rules

- Periodic cgroup samples are metrics, not trace events.
- Trace events are allowed for cgroup anomalies and final summaries only.
- Metric label cardinality must be controlled through an allowlist. Allowed
  labels are bounded categories such as operation name, workspace phase,
  cgroup target kind, status, bounded reason, bounded error kind, and resource
  kind.
- Do not promote `request_id`, `workspace_session_id`, `command_session_id`,
  PIDs, raw paths, path-derived IDs, raw root hashes, command text, stdin,
  output, auth tokens, env values, raw workspace roots, raw cgroup paths, raw
  layer paths, raw error strings, or arbitrary DTO fields to metric labels.
- PID metrics may include aggregate counts such as current/peak/count; they
  must not include sampled PID lists.
- Latency metrics use span durations or direct histograms, not subtraction of
  unrelated event timestamps and not command response timing fields.
- Command final cgroup metric mapping must read a deterministic final sample.
  Preserve the final-sample-before-cleanup ordering that prevents post-cleanup
  periodic samples from affecting retained final-sample enrichment.
- Dashboards must read metrics from the collector/backend, not from
  `cli_operation_specs`.
- Dashboard validation must load the JSON against a chosen local Grafana stack
  with a Prometheus-compatible metrics datasource. Tempo may be used only for
  trace panels, not as the metrics datasource.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| Metrics config and validation | 80 to 140 |
| Daemon metrics exporter/registry wiring | 220 to 360 |
| Narrow runtime metrics recorder interface and call sites | 180 to 340 |
| Cgroup metric mapping and allowlist | 160 to 300 |
| Dashboard JSON/provisioning validation | 220 to 420 |
| Tests | 170 to 380 |
| Phase 4a total | 950 to 1,800 |

## Acceptance Criteria

- [ ] Runtime operation latency histograms exist.
- [ ] Workspace phase latency histograms exist.
- [ ] Publish rejection counters include bounded reason labels.
- [ ] Remount failure counters include bounded reason labels.
- [ ] Command cancellation counters include bounded reason labels.
- [ ] Command final cgroup sample/cleanup ordering cannot let a post-cleanup
      periodic sample affect final CPU delta/percent enrichment.
- [ ] Cgroup periodic CPU/memory/pids/pressure/disk samples export as metrics.
- [ ] Metrics are emitted from internal sample/finalization/cleanup boundaries,
      not by polling `inspect_cgroup_monitor` or `read_cgroup_monitor_samples`.
- [ ] No periodic cgroup sample trace events are emitted.
- [ ] Dashboards use collector/backend metrics and do not call
      `inspect_cgroup_monitor` or `read_cgroup_monitor_samples`.
- [ ] Existing `inspect_cgroup_monitor` and `read_cgroup_monitor_samples`
      behavior is unchanged in this phase.
- [ ] Metric labels are allowlisted and exclude raw paths, path-derived IDs,
      request IDs, workspace session IDs, command session IDs, PIDs, PID lists,
      raw root hashes, command text, stdin, output, auth tokens, env values,
      raw workspace roots, raw cgroup paths, raw layer paths, and free-form
      error strings.
- [ ] Dashboard files load in the chosen local Grafana stack with the configured
      metrics datasource; any Tempo panels are trace-only.
- [ ] `cargo test -p sandbox-daemon -p sandbox-runtime -p sandbox-runtime-workspace -p sandbox-runtime-command`
      passes.
