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
- Assert safe-field behavior for request and command instrumentation.

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
    pub sink: TelemetrySink,
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

## Instrumentation Rules

- Root daemon span: `daemon.request`.
- Runtime dispatch span: `runtime.<operation>`, starting with command ops.
- Command spans: `runtime.exec_command`, `runtime.write_command_stdin`,
  `runtime.read_command_lines`, `command.spawn`, `command.wait_initial_yield`,
  and `command.finalize`.
- `runtime.exec_command` must not contain workspace capture, layerstack publish,
  or remount child spans.
- Use `#[instrument(skip(...))]` or explicit spans so request/input structs are
  not auto-captured.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| Workspace and crate dependencies | 8 to 18 |
| Config schema and baseline YAML | 100 to 160 |
| Daemon telemetry setup | 140 to 230 |
| Daemon serve/server identity plumbing | 70 to 120 |
| Daemon dispatch spans and safe fields | 50 to 90 |
| Runtime command spans | 80 to 140 |
| Tests | 70 to 120 |
| Total | 520 to 820 |

## Acceptance Criteria

- [ ] `config/prd.yml` defaults telemetry to disabled.
- [ ] `daemon.telemetry.sink.kind = local_json` works with
      `stream = stdout` or `stream = stderr` only in foreground mode.
- [ ] Local JSON stream telemetry is rejected with `sandbox-daemon serve
      --spawn` unless a deliberate capture path exists.
- [ ] `daemon.request` includes `request_id`, `operation`, sanitized scope, and
      dynamic `sandbox_id` when available.
- [ ] Root/request spans do not include raw `Request.args`.
- [ ] Command spans do not include command text, stdin, command output, auth
      tokens, environment values, or raw workspace roots.
- [ ] `runtime.exec_command` span only covers live command work and one-shot
      workspace cleanup when applicable.
- [ ] Existing command transcripts are unchanged.
- [ ] No response envelope or protocol metadata change is introduced.
- [ ] `cargo fmt --check` passes.
- [ ] `cargo test -p sandbox-daemon -p sandbox-runtime` passes.
