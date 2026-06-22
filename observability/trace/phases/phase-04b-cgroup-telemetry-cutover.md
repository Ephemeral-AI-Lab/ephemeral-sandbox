# Phase 4b: Cgroup Telemetry Cutover

## Goal

After Phase 4a adds the metrics path and dashboard definitions for cgroup
stats, remove the CLI/catalog-facing cgroup monitor operation surface and keep
cgroup stats on the telemetry path.

This is intentionally a compatibility-breaking cleanup phase. Do not hide it
inside tracing setup, OTLP export, dashboard work, or live Grafana validation.
Phase 4c owns live observability-stack testing and example capture after this
cutover lands.

## Scope

- Move `crates/sandbox-runtime/operation/src/public/cgroup_monitor` to an
  internal service, or collapse the remaining code into internal workspace and
  command telemetry adapters.
- Remove `inspect_cgroup_monitor` and `read_cgroup_monitor_samples` from runtime
  `cli_operation_specs`, CLI operation families, operation entries,
  daemon-described runtime catalog output, and gateway-rendered runtime help.
- Remove gateway CLI mappings and help text for cgroup monitor operations.
- Replace public operation tests with internal registry/metrics tests.
- Keep `CgroupMonitorSample` as an internal typed source for metrics, final
  summaries, cleanup state, and anomaly detection.

## File And Folder Structure Changes

```text
crates/sandbox-runtime/operation/src/
  public/
    mod.rs                         # drop cgroup_monitor family/spec/entry chains
    cgroup_monitor/                # remove or move
  internal/
    services.rs                    # refer to internal cgroup monitor service
    cgroup_monitor/                # new internal home, if a service module remains
      core.rs
      error.rs
      service/
        contract.rs                # private, only if still useful for tests
        types.rs                   # private, only if still useful for tests

crates/sandbox-runtime/operation/src/lib.rs
  # stop re-exporting public cgroup_monitor and public CgroupMonitorOperationService

crates/sandbox-runtime/operation/tests/
  cgroup_monitor_operations.rs     # remove public operation/catalog tests
  cgroup_monitor_metrics.rs        # new or focused internal metrics tests
  service_graph.rs                 # update expected public runtime operations

crates/sandbox-gateway/tests/
  gateway_cli.rs                   # remove cgroup monitor CLI catalog mappings

observability/trace/
  README.md
  phases/phase-04b-cgroup-telemetry-cutover.md

docs/README/
  sandbox-runtime.md
```

Do not remove workspace/command cgroup lifecycle recording. The cutover only
removes the public operation-spec read surface.

## Struct/Class And Field Changes

Public operation DTOs removed from the runtime crate export surface:

```rust
pub struct InspectCgroupMonitorInput { /* removed from public API */ }
pub struct ReadCgroupMonitorSamplesInput { /* removed from public API */ }
pub struct InspectCgroupMonitorOutput { /* removed from public API */ }
pub struct ReadCgroupMonitorSamplesOutput { /* removed from public API */ }
```

Internal data that remains:

```rust
pub(crate) struct CgroupMonitorOperationService {
    /* internal service, if still useful */
}

// From sandbox-runtime-workspace, still used as the typed metrics source.
pub struct CgroupMonitorSample {
    /* existing fields */
}
```

No `sandbox_protocol::Response` changes are required. The operations disappear
from the catalog instead of returning a new response shape.

This is the response simplification for cgroup resource data: raw targets,
retained sample windows, PID lists, CPU/memory/IO/pressure/disk snapshots, and
cleanup diagnostic strings stop being reachable through runtime operation
responses. Metrics carry the resource series; trace events carry only
anomalies, final summaries, cleanup status, and bounded error classes.

## Cutover Rules

- Phase 4b starts only after Phase 4a has the telemetry metrics path and
  dashboard definitions in place.
- Do not treat live Grafana testing or live example capture as Phase 4b scope.
  That validation belongs to Phase 4c.
- Do not remove `CgroupMonitorRegistry`, session final samples, command final
  samples, cleanup state, or retained internal samples needed for metrics.
- Do not leave `inspect_cgroup_monitor` or `read_cgroup_monitor_samples` in
  `cli_operation_specs` as hidden old-name operations.
- Do not replace the removed operations with a new response payload that mirrors
  the old cgroup sample shape. The telemetry backend is the canonical stats
  interface after this phase.
- If a direct debug read surface is still required, define it as a separate
  product/debug API outside the telemetry stats path before deleting the old
  operation specs.
- Gateway help and catalog mapping must no longer advertise cgroup monitor
  operations after this phase.
- Manager catalog output is not a cgroup-removal target because manager specs
  describe manager operations. The relevant external surfaces are the runtime
  catalog described by the daemon/runtime facade and gateway-rendered runtime
  help.

## LOC Estimate

| Area | Changed LOC |
| --- | ---: |
| Move or collapse operation cgroup monitor modules | -80 to +80 |
| Public export/catalog removal | -40 to -90 |
| Gateway CLI mapping/help cleanup | -40 to -90 |
| Test rewrite from public operations to internal metrics/registry behavior | 180 to 320 |
| Docs/spec updates | 20 to 40 |
| Total churn | 300 to 560 |

Net LOC should be negative to small positive. This is a compatibility-breaking
deletion phase, so review changed-LOC/churn separately from net LOC. The
implementation should still prove metrics/final samples remain correct.

## Acceptance Criteria

- [x] `inspect_cgroup_monitor` is absent from runtime `cli_operation_specs`,
      CLI operation families, operation entries, daemon-described runtime
      catalog output, gateway-rendered runtime help, and gateway CLI mappings.
- [x] `read_cgroup_monitor_samples` is absent from runtime `cli_operation_specs`,
      CLI operation families, operation entries, daemon-described runtime
      catalog output, gateway-rendered runtime help, and gateway CLI mappings.
- [x] `sandbox_runtime::cgroup_monitor` is no longer exported as a public
      operation module.
- [x] `docs/README/sandbox-runtime.md` no longer lists cgroup monitor operations
      as an external runtime operation surface.
- [x] Old cgroup monitor response shapes, including raw targets, cgroup paths,
      retained sample windows, PID lists, and cleanup error strings, are not
      reachable through runtime operation responses.
- [x] Existing workspace/session/command cgroup lifecycle recording still works.
- [x] Command final samples still feed metrics, and session final cleanup state
      still feeds bounded telemetry final summaries.
- [x] Internal metrics/registry tests prove command final samples and session
      final cleanup state remain available to telemetry after operation removal.
- [x] No hidden old-name operation is left for the old cgroup monitor
      operation names.
- [x] `cargo test -p sandbox-runtime -p sandbox-gateway` passes.
