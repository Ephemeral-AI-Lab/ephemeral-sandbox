# Phase 5: Runner Context Propagation

## Goal

Propagate W3C trace context from `sandbox-daemon serve` into `sandbox-daemon
ns-runner` so runner spans can attach to daemon command spans when context is
present.

## Scope

- Add context extraction/injection at every daemon-to-runner launch boundary:
  command execution and workspace overlay/remount setns-runner launches.
- Add trace context fields to the daemon-to-runner internal DTO.
- Add runner spans for setns, cgroup join, overlay mount/remount, and command
  execution.
- Preserve standalone runner behavior when trace context is absent.

## File And Folder Structure Changes

```text
crates/sandbox-runtime/namespace-process/
  Cargo.toml
  src/
    runner/
      protocol.rs
      setns.rs
      shell_exec.rs
      shell_exec/
        wait.rs
    holder/
      network.rs              # only if network setup span is required
  tests/
    unit/
      runner/
        trace_context.rs      # new

crates/sandbox-runtime/command/
  Cargo.toml
  src/
    process.rs              # command NamespaceRunnerRequest producer
    pty.rs
  tests/
    unit/
      process.rs

crates/sandbox-runtime/workspace/src/namespace/
  setns_runner.rs           # overlay/remount NamespaceRunnerRequest producer
crates/sandbox-runtime/workspace/tests/
  unit/
    setns_runner.rs         # new or focused additions

crates/sandbox-daemon/
  src/
    runner.rs
    serve.rs
    telemetry.rs
  tests/
    unit/
      runner.rs
      telemetry.rs
```

Do not route runner spans through the command transcript or manager/gateway RPC.

## Struct/Class And Field Changes

```rust
pub struct NamespaceRunnerRequest {
    pub request_id: String,
    pub args: serde_json::Value,
    pub workspace_root: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    #[serde(default)]
    pub upperdir: Option<PathBuf>,
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    #[serde(default)]
    pub ns_fds: Option<NsFds>,
    #[serde(default)]
    pub cgroup_path: Option<PathBuf>,
    #[serde(default)]
    pub timeout_seconds: Option<f64>,
    #[serde(default)]
    pub trace_context: Option<TraceContext>,
}

pub struct TraceContext {
    pub traceparent: String,
    pub tracestate: Option<String>,
}
```

If environment variables are used instead of DTO fields, the accepted variables
must be limited to W3C context keys such as `TRACEPARENT` and `TRACESTATE`; do
not pass command payloads or auth values through telemetry context.

`trace_context` must be optional and `#[serde(default)]` so existing
daemon-to-runner payloads decode unchanged. Missing or invalid context changes
only telemetry parentage; it must not change command exit status, runner result
payloads, transcript rows, response shape, or cleanup behavior.

## Instrumentation Rules

- Runner child spans attach to daemon command spans when valid context exists.
- Runner spans stand alone when context is absent or invalid.
- Context propagation must not become a command correctness dependency.
- Only W3C `traceparent` and `tracestate` may cross the runner boundary as
  trace context. Do not use raw request/runner DTOs, command args, cwd,
  workspace roots, layer paths, cgroup paths, environment, auth values, stdin,
  or command output as span fields/events or propagation metadata.
- Runtime crates may carry and inject/extract W3C strings, but they must not
  initialize subscribers, configure exporters, or own OTel SDK/provider setup.
- Runner spans must not emit raw command text, stdin, output, environment
  values, auth tokens, raw workspace roots, raw cgroup paths, or raw layer paths.
  They must also avoid raw `Debug`/`Display` errors and raw remount report JSON.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| Internal DTO/context fields | 40 to 80 |
| Command and workspace launcher context injection | 130 to 230 |
| Runner context extraction | 80 to 140 |
| Runner spans | 110 to 190 |
| Tests | 160 to 250 |
| Total | 520 to 850 |

## Acceptance Criteria

- [ ] Daemon command span injects valid W3C context into command runner launch
      material.
- [ ] Workspace overlay/remount setns-runner launches inject valid W3C context
      when a parent span exists.
- [ ] `ns-runner` extracts context and creates child spans when context exists.
- [ ] `ns-runner` creates standalone spans when context is absent.
- [ ] Invalid context is ignored or reported as a telemetry diagnostic without
      failing command execution.
- [ ] Old runner request payloads without `trace_context` still deserialize.
- [ ] Missing or invalid context does not change command exit status, runner
      result JSON, transcript rows, cleanup behavior, or
      `sandbox_protocol::Response` shape.
- [ ] Command transcript behavior is unchanged.
- [ ] No context propagation field leaks command payloads, env values, auth
      tokens, raw args, raw `Debug`/`Display` errors, raw workspace roots, raw
      cgroup paths, raw layer paths, raw remount report JSON, or command output.
- [ ] Standalone runner behavior means a valid existing FD/config launch without
      trace context, not a runner launch missing required request/result FDs or
      config env.
- [ ] Runtime crates do not initialize subscribers, configure exporters, or own
      OTel SDK/provider setup.
- [ ] `cargo test -p sandbox-daemon -p sandbox-runtime-command -p sandbox-runtime-workspace -p sandbox-runtime-namespace-process -p sandbox-runtime`
      passes, including focused transcript tests.
- [ ] End-to-end command test proves daemon and runner spans share a trace when
      context propagation is enabled.
