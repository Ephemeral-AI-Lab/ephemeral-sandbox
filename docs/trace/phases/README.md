# Trace Implementation Phase Specs

These phase specs break `docs/trace/README.md` into implementation-sized
batches. Each phase is intended to be implemented and reviewed independently.

| Phase | Spec | Primary Outcome | Estimated Net LOC |
| --- | --- | --- | ---: |
| 1 | [Local JSON Tracing](phase-01-local-json.md) | Local subscriber, root spans, safe field policy, command spans | 520 to 820 |
| 2 | [Runtime Semantic Spans](phase-02-runtime-semantic-spans.md) | Workspace, remount, publish, and cgroup anomaly events | 360 to 620 |
| 3 | [OTLP Export](phase-03-otlp-export.md) | Single-path OTLP export, bounded failure behavior, shutdown flush | 420 to 760 |
| 4a | [Metrics And Dashboards](phase-04-metrics-dashboards.md) | Metrics-first cgroup samples and dashboards while keeping current public cgroup read ops | 650 to 1,100 |
| 4b | [Cgroup Telemetry Cutover](phase-04b-cgroup-telemetry-cutover.md) | Move cgroup stats out of CLI/catalog operation specs after telemetry is canonical | 300 to 560 |
| 5 | [Runner Context Propagation](phase-05-runner-context.md) | W3C context propagation into `ns-runner` | 380 to 650 |

Global constraints for every phase:

- [ ] Do not add `crates/sandbox-runtime-trace/`.
- [ ] Do not add `crates/sandbox-runtime/operation/src/internal/telemetry.rs`.
- [ ] Runtime crates emit inline `tracing` spans/events only.
- [ ] `sandbox-daemon` owns subscriber/exporter setup.
- [ ] Production traces use one OTLP path only.
- [ ] Do not change `sandbox_protocol::Response` before the protocol phase.
- [ ] Do not emit raw command text, stdin, command output, auth tokens, raw env
      values, raw request args, or raw workspace roots.
