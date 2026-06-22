# Phase 5: Runner Context Propagation

## Goal

Propagate W3C trace context from `sandbox-daemon serve` into `sandbox-daemon
ns-runner` so runner spans can attach to daemon command spans when context is
present.

## Scope

- Add context extraction/injection at the daemon command launch boundary.
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
    process.rs
    pty.rs
  tests/
    unit/
      process.rs

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
pub struct NamespaceCommandRequest {
    pub request_id: String,
    pub args: serde_json::Value,
    pub workspace_root: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    pub upperdir: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    pub ns_fds: Option<NsFds>,
    pub cgroup_path: Option<PathBuf>,
    pub timeout_seconds: Option<f64>,
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

## Instrumentation Rules

- Runner child spans attach to daemon command spans when valid context exists.
- Runner spans stand alone when context is absent or invalid.
- Context propagation must not become a command correctness dependency.
- Runner spans must not emit raw command text, stdin, output, environment
  values, auth tokens, raw workspace roots, raw cgroup paths, or raw layer paths.

## LOC Estimate

| Area | Net LOC |
| --- | ---: |
| Internal DTO/context fields | 40 to 80 |
| Command launcher context injection | 80 to 140 |
| Runner context extraction | 70 to 120 |
| Runner spans | 90 to 150 |
| Tests | 100 to 160 |
| Total | 380 to 650 |

## Acceptance Criteria

- [ ] Daemon command span injects valid W3C context into runner launch material.
- [ ] `ns-runner` extracts context and creates child spans when context exists.
- [ ] `ns-runner` creates standalone spans when context is absent.
- [ ] Invalid context is ignored or reported as a telemetry diagnostic without
      failing command execution.
- [ ] Command transcript behavior is unchanged.
- [ ] No context propagation field leaks command payloads, env values, auth
      tokens, raw workspace roots, raw cgroup paths, or raw layer paths.
- [ ] `cargo test -p sandbox-runtime-command -p sandbox-runtime-namespace-process`
      passes.
- [ ] End-to-end command test proves daemon and runner spans share a trace when
      context propagation is enabled.
