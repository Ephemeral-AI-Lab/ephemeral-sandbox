# Phase 4c Live Telemetry Validation

Validated at 2026-06-22T20:00:49Z against the shared observability stack.

## Live Workload

- Stack: shared compose stack with OpenTelemetry Collector, Prometheus-compatible metrics storage, Tempo, and Grafana.
- Sandbox privileges: minimal admin capability run using `SYS_ADMIN` and `NET_ADMIN`; no privileged container mode.
- Sandbox storage: tmpfs-backed state and scratch roots inside the sandbox.
- Runtime activity: workspace create, command load, runtime `exec_command` latency path, remount, publish conflict, workspace destroy.
- Cgroup targets: session and command.
- Publish conflict: observed.

## Prometheus Query Summary

| Metric family | Live series | Bounded label keys observed | Aggregate values |
| --- | ---: | --- | --- |
| `sandbox_runtime_operation_latency_ms_milliseconds_count` | 1 | `operation`, `status` | `exec_command` `ok` count `1` |
| `sandbox_workspace_phase_latency_ms_milliseconds_count` | 5 | `workspace_phase`, `status` | create, destroy, publish, remount, rejected publish counts |
| `sandbox_publish_rejections_total` | 1 | `bounded_reason` | `source_conflict` count `1` |
| `sandbox_cgroup_cpu_usage_usec_microseconds` | 2 | `cgroup_target_kind`, `resource_kind`, `status` | command `1111`, session `975568` |
| `sandbox_cgroup_memory_current_bytes` | 2 | `cgroup_target_kind`, `resource_kind`, `status` | command `86016`, session `577536` |
| `sandbox_cgroup_pids_current` | 2 | `cgroup_target_kind`, `resource_kind`, `status` | command `0`, session `1` |
| `sandbox_cgroup_pressure_some_total_usec_microseconds` | 6 | `cgroup_target_kind`, `resource_kind`, `status` | command and session CPU, IO, memory pressure |
| `sandbox_cgroup_disk_upperdir_bytes` | 2 | `cgroup_target_kind`, `resource_kind`, `status` | command `524288`, session `524288` |
| `sandbox_cgroup_read_errors_total` | 0 | none | no cgroup read errors |

Label-key scan across required live families found only bounded metric labels:
`bounded_reason`, `cgroup_target_kind`, `exported_instance`, `exported_job`,
`instance`, `job`, `operation`, `resource_kind`, `status`, and
`workspace_phase`. No request IDs, workspace session IDs, command session IDs,
PIDs, path-derived IDs, raw paths, root hashes, command text, stdin, output,
env/auth values, raw cgroup paths, or free-form error labels were present.

## Dashboard Validation

Grafana opened the provisioned dashboards for command latency, publish
conflicts, remount health, and cgroup resources. Grafana API metadata confirmed
all panels use the `prometheus` datasource.

Panel query results with live data:

- Command latency: runtime operation average latency returned one
  `exec_command` `ok` series.
- Publish conflicts: publish rejections returned one `source_conflict` series;
  publish phase average latency returned `ok` and `rejected` series.
- Remount health: remount phase average latency returned one `ok` series.
  Remount failures returned no series, matching the successful live run.
- Cgroup resources: CPU percent, memory current, PIDs current, pressure avg10,
  and disk upperdir bytes returned live command/session series. Cgroup read
  errors returned no series.

## Removed Operation Surface

Runtime catalog, runtime dispatch, gateway runtime catalog, and gateway runtime
help checks passed with `inspect_cgroup_monitor` and
`read_cgroup_monitor_samples` omitted. Dashboard and stack query scans found no
dependency on those operation names outside guard tests.

## Cleanup

Stop the local stack with the shared compose file. Remove disposable validation
driver/cache directories created outside the repo. The live workload removes its
sandbox state root after telemetry shutdown.
