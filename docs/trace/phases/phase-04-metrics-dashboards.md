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
    metrics.rs              # optional split if telemetry.rs grows too large

crates/sandbox-runtime/config/src/configs/
  daemon.rs

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
crates still must not own exporter setup.

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
typed metrics source while still supporting the existing direct read API.

## Metric Rules

- Periodic cgroup samples are metrics, not trace events.
- Trace events are allowed for cgroup anomalies and final summaries only.
- Prometheus/Loki label cardinality must be controlled. Do not promote
  `request_id`, `command_session_id`, raw paths, or raw root hashes to metric
  labels.
- Latency metrics use span durations or direct histograms, not subtraction of
  unrelated event timestamps.
- Dashboards must read metrics from the collector/backend, not from
  `operation_specs`.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| Metrics config and validation | 80 to 140 |
| Daemon metrics exporter/registry wiring | 160 to 260 |
| Runtime metric emission call sites | 140 to 260 |
| Cgroup metric mapping | 100 to 180 |
| Dashboard JSON | 120 to 220 |
| Tests | 50 to 100 |
| Phase 4a total | 650 to 1,100 |

## Acceptance Criteria

- [ ] Runtime operation latency histograms exist.
- [ ] Workspace phase latency histograms exist.
- [ ] Publish rejection counters include bounded reason labels.
- [ ] Remount failure counters include bounded reason labels.
- [ ] Command cancellation counters include bounded reason labels.
- [ ] Cgroup periodic CPU/memory/pids/pressure/disk samples export as metrics.
- [ ] No periodic cgroup sample trace events are emitted.
- [ ] Dashboards use collector/backend metrics and do not call
      `inspect_cgroup_monitor` or `read_cgroup_monitor_samples`.
- [ ] Existing `inspect_cgroup_monitor` and `read_cgroup_monitor_samples`
      behavior is unchanged in this phase.
- [ ] Metric labels exclude raw paths, request IDs, command session IDs, command
      text, stdin, output, auth tokens, env values, and raw workspace roots.
- [ ] Dashboard files load in the chosen local Grafana/Tempo stack.
- [ ] `cargo test -p sandbox-daemon -p sandbox-runtime -p sandbox-runtime-workspace`
      passes.
