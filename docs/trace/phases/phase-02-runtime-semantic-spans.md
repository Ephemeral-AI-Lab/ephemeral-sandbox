# Phase 2: Runtime Semantic Spans

## Goal

Add semantic spans/events for workspace, remount, layerstack publish, and cgroup
monitor anomalies at stable live runtime boundaries. This phase expands
instrumentation depth without creating a custom event-object pipeline or
instrumenting private helper functions as public span names.

## Scope

- Add workspace create/capture/destroy/remount spans.
- Emit workspace create phase events from the existing internal setup phase
  timers in `WorkspaceModeManager::initialize_handle`.
- Emit remount verification events from existing `RemountOverlayResult` and
  allowlisted facts at the live setns-runner boundary. Do not project the raw
  namespace-process remount report JSON.
- Emit layerstack publish facts and OCC events from operation-level wrappers.
- Emit cgroup anomaly and final-summary events only.

## File And Folder Structure Changes

```text
crates/sandbox-runtime/operation/src/
  internal/workspace_session/service/impls/
    create_workspace_session.rs
    capture_session_changes.rs
    destroy_session.rs
    apply_and_finish_remount.rs
    resolve_session.rs
  internal/workspace_remount/service/impls/
    remount_workspace_session.rs
  internal/layerstack/service/impls/
    publish_changes.rs

crates/sandbox-runtime/workspace/src/
  service/impls/
    create_workspace.rs
    capture_changes.rs
    destroy_workspace.rs
    remount_workspace.rs
  lifecycle/
    create.rs
    destroy.rs
    remount/
      result.rs
  namespace/
    setns_runner.rs
  namespace/cgroup_monitor.rs

crates/sandbox-runtime/namespace-process/src/runner/
  setns.rs                    # source of remount diagnostics; do not emit raw report JSON

crates/sandbox-runtime/operation/src/public/command/service/
  finalize.rs                 # command final-sample handoff boundary

crates/sandbox-runtime/operation/tests/
  trace_workspace.rs       # new, or focused additions to workspace_session.rs
  trace_publish.rs         # new, or focused additions to layerstack_publish.rs
  trace_cgroup.rs          # new, focused on anomaly/final-summary events
```

Do not add a new shared trace helper module under runtime.

## Struct/Class And Field Changes

Expected production structs do not need new externally visible fields in this
phase. Instrumentation reads live result fields where they exist, and emits
workspace create phase timings at the internal setup boundary:

```rust
WorkspaceHandle {
    profile,
    /* existing fields */
}

RemountOverlayResult {
    mount_verified,
    failure_summary,
}

PublishChangesResult {
    revision,
    route_summary,
    no_op,
    /* existing fields */
}
```

If test-only capture of emitted events needs helper types, keep them in tests or
daemon-owned telemetry test support. Do not expose runtime telemetry DTOs.

## Instrumentation Rules

- Stable span names:
  - `workspace.create_session`
  - `workspace.destroy_session`
  - `workspace.capture_changes`
  - `workspace.remount`
  - `layerstack.publish_changes`
- Stable cgroup event names:
  - `cgroup_monitor.anomaly`
  - `cgroup_monitor.final_summary`
- Do not create spans named after private helpers such as `plan_publish`,
  `validate_source_paths`, or manifest commit internals.
- OCC events may include counts, versions, root-hash match booleans,
  fingerprint kinds, redacted path class, or keyed path/root tokens when
  correlation is required. They must not include raw host paths, raw root
  hashes, raw layer paths, or path-derived IDs.
- Cgroup periodic samples remain typed state/API data and later metrics data.
  Trace events are limited to anomalies and final summaries.
- Do not add trace work to CLI-facing cgroup operation specs if cgroup stats are
  being moved to telemetry. Keep the trace work at the internal lifecycle,
  command-final, cleanup, and anomaly boundaries.
- Cgroup trace boundaries are the internal registry's session-final,
  command-final, cleanup, and anomaly points, plus the command finalization
  handoff that records final samples. Periodic sampler ticks and public read
  operations are not telemetry boundaries.
- Do not project command response timing fields, cgroup monitor response
  payloads, remount diagnostic JSON, `WorkspaceHandle`, `WorkspaceEntry`,
  `PublishChangesResult`, `Debug` structs, or raw `Display` errors wholesale.
  Trace only explicit safe fields, counts, booleans, statuses, bounded reasons,
  and bounded error classes.
- Reassert the global constraints from `phases/README.md` in this phase's
  review: no runtime trace crate, no runtime telemetry module, no subscriber or
  exporter setup in runtime crates, no response-envelope change, and no raw
  sensitive fields.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| Workspace session spans/events | 80 to 140 |
| Workspace service/lifecycle phase events | 60 to 120 |
| Remount orchestration/result events | 90 to 170 |
| Layerstack publish/OCC events | 70 to 120 |
| Cgroup anomaly/final summary events | 70 to 120 |
| Tests | 120 to 170 |
| Total | 500 to 800 |

## Acceptance Criteria

- [ ] Workspace create/destroy/capture/remount spans align with live call paths.
- [ ] Workspace create phase events use existing internal explicit `Instant`
      phase timings and preserve `WorkspaceHandle` behavior.
- [ ] Remount events preserve `RemountOverlayResult` behavior and read only
      allowlisted booleans/counts/statuses from the live setns-runner boundary.
- [ ] No `lifecycle/remount/report.rs` file or replacement report DTO is
      introduced for telemetry.
- [ ] Layerstack publish emits structured result/rejection/OCC events
      on the normal tracing path without a runtime trace object API.
- [ ] No span name mirrors private helper functions unless the helper has been
      promoted to a stable diagnostic boundary in the same change.
- [ ] Cgroup periodic samples do not emit trace events.
- [ ] Cgroup trace events are limited to anomalies and final summaries.
- [ ] `inspect_cgroup_monitor` and `read_cgroup_monitor_samples` are not added
      as span names or new instrumentation boundaries.
- [ ] Raw paths, raw root hashes, layer paths, cgroup paths, command text,
      stdin, output, env values, auth tokens, raw DTO `Debug`, raw response
      payloads, and raw error strings are not emitted.
- [ ] `WorkspaceHandle`, `WorkspaceEntry`, `PublishChangesResult`, remount
      diagnostic JSON, and cgroup monitor samples are never auto-captured as
      telemetry fields.
- [ ] Global forbidden path/module and no-`Response`-change checks pass for this
      phase.
- [ ] `cargo test -p sandbox-runtime` passes.
- [ ] If workspace/layerstack crates are touched,
      `cargo test -p sandbox-runtime-workspace` and
      `cargo test -p sandbox-runtime-layerstack` pass.
- [ ] If namespace-process remount diagnostics are touched,
      `cargo test -p sandbox-runtime-namespace-process` passes.
