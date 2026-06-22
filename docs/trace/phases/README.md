# Trace Implementation Phase Specs

These phase specs break `docs/trace/README.md` into implementation-sized
batches. Each phase is intended to be implemented and reviewed independently.

| Phase | Spec | Primary Outcome | Estimated Changed LOC |
| --- | --- | --- | ---: |
| 1 | [Local JSON Tracing](phase-01-local-json.md) | Local subscriber, root spans, safe field policy, command spans | 650 to 1,020 |
| 2 | [Runtime Semantic Spans](phase-02-runtime-semantic-spans.md) | Workspace, remount, publish, and cgroup anomaly events | 500 to 800 |
| 3 | [OTLP Export](phase-03-otlp-export.md) | Single-path OTLP export, bounded failure behavior, tracked shutdown flush | 520 to 910 |
| 4a | [Metrics And Dashboards](phase-04-metrics-dashboards.md) | Metrics-first cgroup samples and dashboards while keeping current public cgroup read ops | 950 to 1,800 |
| 4b | [Cgroup Telemetry Cutover](phase-04b-cgroup-telemetry-cutover.md) | Move cgroup stats out of CLI/catalog operation specs after telemetry is canonical | 300 to 560 churn, net negative to small positive |
| 5 | [Runner Context Propagation](phase-05-runner-context.md) | W3C context propagation into `ns-runner` | 520 to 850 |

Global constraints for every phase:

- [ ] Do not add `crates/sandbox-runtime-trace/`.
- [ ] Do not add `crates/sandbox-runtime/operation/src/internal/telemetry.rs`.
- [ ] Runtime crates emit inline `tracing` spans/events only.
- [ ] `sandbox-daemon` owns subscriber/exporter setup.
- [ ] Production traces use one OTLP path only.
- [ ] Do not change `sandbox_protocol::Response` before the protocol phase.
- [ ] Do not emit raw command text, stdin, command output, auth tokens, raw env
      values, raw request args, raw host paths, raw workspace roots, raw cgroup
      paths, raw layer paths, raw upper/work dirs, raw transcript/artifact
      paths, raw PIDs, or raw root hashes.
- [ ] Do not project protocol responses, raw `Debug` structs, or raw `Display`
      error strings wholesale into telemetry.
- [ ] Do not emit full request/runner DTOs, cgroup sample DTOs, remount report
      JSON, workspace handles/entries, or layerstack results as telemetry
      fields/events.
- [ ] Metric labels must be low-cardinality allowlisted values only. Do not use
      `request_id`, `workspace_session_id`, `command_session_id`, PIDs,
      root hashes, path-derived IDs, raw paths, command payloads, env/auth
      values, or free-form error strings as labels.
- [ ] Do not add trace spans for public cgroup monitor read operations; cgroup
      trace events are internal anomalies and final summaries only.
- [ ] Do not use command response timing fields as operation latency; use span
      durations or direct histograms.
- [ ] Each phase that touches telemetry must include negative tests or a guard
      covering these constraints, including the absence of forbidden paths and
      modules.
