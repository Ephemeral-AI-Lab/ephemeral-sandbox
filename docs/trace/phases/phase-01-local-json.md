# Phase 1: Local JSON Tracing

## Goal

Add local, foreground-only JSON tracing for daemon and runtime operation
boundaries without adding OTLP, protocol metadata, gateway UX, dashboards, or
custom runtime trace infrastructure.

## Scope

- Add `tracing` to crates that emit spans/events.
- Add `tracing-subscriber` to `sandbox-daemon`.
- Add daemon telemetry config for disabled and local JSON stream modes.
- Add dynamic daemon `sandbox_id` identity plumbing.
- Add `daemon.request` and runtime command root spans.
- Assert safe-field behavior for request, response, error, and command
  instrumentation.
- Add negative tests for forbidden telemetry fields and forbidden cgroup public
  read-operation spans.

## File And Folder Structure Changes

```text
Cargo.toml
  [workspace.dependencies]
    tracing
    tracing-subscriber

config/prd.yml
  daemon.telemetry default-disabled config

crates/sandbox-runtime/config/src/configs/
  daemon.rs
crates/sandbox-runtime/config/tests/unit/configs/
  daemon.rs

crates/sandbox-daemon/
  Cargo.toml
  src/
    main.rs
    telemetry.rs              # new daemon-owned subscriber setup
    serve.rs
    server/
      dispatch.rs
      runtime.rs
  tests/
    unit.rs
    unit/
      telemetry.rs            # new
      serve.rs
      dispatch.rs

crates/sandbox-runtime/operation/
  Cargo.toml
  src/
    public/mod.rs
    public/command/service/impls/
      exec_command.rs
      write_command_stdin.rs
      read_command_lines.rs
    public/command/service/finalize.rs
  tests/
    trace_command.rs          # new, or focused additions to existing tests

crates/sandbox-protocol/tests/
  response_shape.rs           # new or focused no-envelope-change assertion
```

Do not create:

```text
crates/sandbox-runtime-trace/
crates/sandbox-runtime/operation/src/internal/telemetry.rs
```

## Struct/Class And Field Changes

```rust
pub struct DaemonConfig {
    pub server: DaemonServerConfig,
    pub commands: CommandConfig,
    pub cgroup_monitor: CgroupMonitorConfig,
    pub idle_workspace_eviction: IdleWorkspaceEvictionConfig,
    pub telemetry: TelemetryConfig,
}

pub struct TelemetryConfig {
    pub enabled: bool,
    pub service_name: String,
    pub level: String,
    pub sink: Option<TelemetrySink>,
}

pub enum TelemetrySink {
    LocalJson { stream: TelemetryOutputStream },
}

pub enum TelemetryOutputStream {
    Stdout,
    Stderr,
}
```

Add `sandbox_id: Option<String>` to the existing multi-field
`DaemonCliConfig` and `ServerConfig` structs. Do not introduce new wrapper
structs solely to carry sandbox identity.

```rust
pub(crate) struct DaemonCliConfig {
    pub(crate) sandbox_id: Option<String>,
    /* existing fields remain */
}

pub struct ServerConfig {
    pub sandbox_id: Option<String>,
    /* existing fields remain */
}
```

No `sandbox_protocol::Request` or `sandbox_protocol::Response` fields change in
this phase.

Disabled telemetry must deserialize without a sink. When `enabled = true`, the
config validator must require exactly one sink and reject unknown sink kinds,
invalid streams, invalid levels, and local JSON stream mode under detached
`serve --spawn`.

## Instrumentation Rules

- Root daemon span: `daemon.request`.
- Optional generic dispatch span: `runtime.dispatch`. Do not name the dispatch
  span `runtime.<operation>` because that collides with the operation spans and
  would also cover temporary public cgroup read operations.
- Command operation spans: `runtime.exec_command`, `runtime.write_command_stdin`,
  `runtime.read_command_lines`, `command.spawn`, `command.wait_initial_yield`,
  and `command.finalize`.
- Do not emit spans or events for `inspect_cgroup_monitor` or
  `read_cgroup_monitor_samples` in this phase.
- `runtime.exec_command` must not contain workspace capture, layerstack publish,
  or remount child spans.
- Use `#[instrument(skip(...))]` or explicit spans so request/input structs are
  not auto-captured.
- Do not record `sandbox_protocol::Response`, operation response payloads,
  `Debug` structs, or raw `Display` error strings wholesale. Record only
  explicit safe fields such as operation status, bounded error kind, output
  offsets/counts, and command exit status.
- Do not record command text, stdin, stdout/stderr content, raw request args,
  env/auth values, raw host paths, raw workspace roots, raw cgroup paths, raw
  layer paths, raw upper/work dirs, raw transcript/artifact paths, raw PIDs, or
  root hashes. Test fixtures should inject sentinel values for these fields and
  assert they do not appear in telemetry.
- Command finalization must not emit per-sample cgroup state in trace fields.
  Cgroup periodic samples remain API state and later metrics input; Phase 1 may
  emit only bounded finalization status fields.
- Pre-decode request failures, too-large request frames, timeouts, and invalid
  JSON may emit a sanitized `daemon.request` diagnostic without `request_id`,
  but must not include raw payload bytes or auth material.
- Do not treat command response timing fields as operation latency. Local JSON
  span-close timing is the source for operation wall-clock latency in this
  phase.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| Workspace and crate dependencies | 8 to 18 |
| Config schema, validation, and baseline YAML | 140 to 230 |
| Daemon telemetry setup | 140 to 230 |
| Daemon serve/server identity plumbing | 90 to 160 |
| Daemon dispatch spans and safe fields | 80 to 150 |
| Runtime command spans | 80 to 140 |
| Protocol/config/telemetry safe-field tests | 170 to 290 |
| Total | 650 to 1,020 |

## Acceptance Criteria

- [ ] `config/prd.yml` defaults telemetry to disabled.
- [ ] Disabled telemetry config deserializes without a sink; enabled telemetry
      requires exactly one valid sink.
- [ ] `cargo test -p sandbox-runtime-config` covers disabled config, local JSON
      stdout/stderr, invalid stream, invalid level, unknown sink, and spawn
      rejection validation.
- [ ] `daemon.telemetry.sink.kind = local_json` works with
      `stream = stdout` or `stream = stderr` only in foreground mode.
- [ ] Local JSON stream telemetry is rejected with `sandbox-daemon serve
      --spawn` unless a deliberate capture path exists.
- [ ] A manager-started daemon passes dynamic `sandbox_id` to the spawned
      foreground child via argv, env, or an identity file; `ServerConfig`
      receives it; static YAML never contains it.
- [ ] `daemon.request` includes `request_id`, `operation`, sanitized scope, and
      dynamic `sandbox_id` when available.
- [ ] Pre-decode failures, invalid JSON, oversized requests, and timeouts emit
      only sanitized diagnostics and do not require a `request_id`.
- [ ] Root/request spans do not include raw `Request.args`.
- [ ] Runtime spans do not include raw response payloads or raw error details.
- [ ] Command spans do not include command text, stdin, command output, auth
      tokens, environment values, raw host paths, raw workspace roots, raw
      cgroup paths, raw layer paths, raw upper/work dirs, raw transcript or
      artifact paths, PIDs, or root hashes.
- [ ] Sentinel tests assert raw request args, response payloads, command text,
      stdin, stdout/stderr, env/auth-like values, and raw paths never appear in
      local JSON telemetry.
- [ ] No cgroup sample payload fields are emitted from command finalization
      traces.
- [ ] `inspect_cgroup_monitor` and `read_cgroup_monitor_samples` produce no
      spans/events and are absent from trace-name assertions.
- [ ] `runtime.exec_command` span only covers live command work and one-shot
      workspace cleanup when applicable.
- [ ] Existing command transcripts are unchanged.
- [ ] No response envelope or protocol metadata change is introduced.
- [ ] Focused protocol tests prove Phase 1 responses do not gain `result`,
      `meta`, `trace_id`, or telemetry metadata wrappers.
- [ ] Forbidden-path/module guard passes for `crates/sandbox-runtime-trace/` and
      `crates/sandbox-runtime/operation/src/internal/telemetry.rs`.
- [ ] `cargo fmt --check` passes.
- [ ] `cargo test -p sandbox-daemon -p sandbox-runtime -p sandbox-runtime-config -p sandbox-protocol` passes.
