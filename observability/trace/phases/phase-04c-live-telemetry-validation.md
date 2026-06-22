# Phase 4c: Live Telemetry Validation

## Goal

After Phase 4b removes the public cgroup monitor read operations, run the shared
observability stack against live sandbox activity and capture proof that cgroup
stats, runtime latency, publish/remount health, and dashboard panels work without
calling the removed runtime operations.

This phase is validation and evidence capture. It must not reintroduce
`inspect_cgroup_monitor`, `read_cgroup_monitor_samples`, or a replacement
operation response that mirrors cgroup sample payloads.

## Scope

- Start the shared `observability/` stack with OpenTelemetry Collector,
  Prometheus-compatible metrics storage, Tempo when already configured, and
  Grafana.
- Run live sandbox workloads that create workspace/session and command cgroup
  activity, including at least one short CPU or memory load command.
- Query the metrics backend for runtime latency, workspace phase latency,
  remount health, publish conflicts when a conflict fixture exists, and cgroup
  CPU/memory/pids/pressure/disk series.
- Open the provisioned Grafana dashboards and verify panels load from the
  metrics datasource with live data.
- Capture bounded live examples, such as screenshots, sanitized Prometheus query
  output, or a concise validation transcript.
- Confirm dashboards and validation commands do not call
  `inspect_cgroup_monitor` or `read_cgroup_monitor_samples`.
- Document cleanup steps for stopping the local observability stack and removing
  temporary validation artifacts.

## File And Folder Structure Changes

```text
observability/
  docker-compose.yml              # reused shared stack, not phase-specific
  grafana/provisioning/           # reused dashboard and datasource provisioning

observability/trace/
  dashboards/                     # reused dashboard JSON files
  live-examples/
    phase-04c.md                  # optional sanitized run notes
    phase-04c-*.png               # optional screenshots when useful
```

Do not create a separate phase-specific observability directory. New scripts or
runbooks may be added only if they make the live validation repeatable without
embedding host-specific paths, IDs, credentials, or raw cgroup paths.

## Live Validation Rules

- Use the shared `observability/docker-compose.yml` stack.
- Grafana must read metrics through the configured Prometheus-compatible
  datasource. Tempo panels are trace-only.
- Validation must use live sandbox runtime activity, not static dashboard JSON
  loading alone.
- Metric labels must stay low-cardinality and must not include request IDs,
  workspace session IDs, command session IDs, PIDs, path-derived IDs, raw paths,
  root hashes, command text, stdin, output, env/auth values, or free-form error
  strings.
- Cgroup periodic samples must appear as metrics, not per-sample trace events.
- Trace events remain limited to anomalies and final summaries.
- Do not add Loki, log exporters, log panels, derived fields, or trace-to-logs
  correlation in this phase.
- Sanitized live examples may show metric names, bounded labels, panel names,
  timestamps, counts, and aggregate resource values. They must not show raw
  workspace roots, raw cgroup paths, command payloads, auth values, or raw PIDs.

## LOC Estimate

| Area | Changed LOC |
| --- | ---: |
| Live validation runbook or smoke script | 40 to 120 |
| Sanitized example notes/screenshots index | 20 to 80 |
| Dashboard/query validation guards | 60 to 160 |
| Docs/spec updates | 20 to 60 |
| Total churn | 140 to 420 |

## Acceptance Criteria

- [x] The shared observability stack starts from `observability/docker-compose.yml`
      and exposes Grafana plus the metrics backend locally.
- [x] A live sandbox workload emits runtime latency and workspace phase metrics.
- [x] A live sandbox workload emits cgroup CPU, memory, pids, pressure, and disk
      metrics through telemetry.
- [x] Prometheus-compatible queries return non-empty live series for the required
      metric families using only allowlisted labels.
- [x] Grafana loads the command latency, publish conflicts, remount health, and
      cgroup resource dashboards from the provisioned metrics datasource.
- [x] The cgroup resource dashboard shows live data without calling
      `inspect_cgroup_monitor` or `read_cgroup_monitor_samples`.
- [x] Live examples or validation notes are sanitized and contain no raw paths,
      IDs, PIDs, command text, command output, env/auth values, or raw cgroup
      paths.
- [x] Runtime help/catalog output still omits the removed cgroup monitor
      operations during live validation.
- [x] Local cleanup steps are documented and leave no required background stack
      running after validation.
