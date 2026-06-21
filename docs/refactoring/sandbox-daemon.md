# sandbox-daemon Crate Spec

## Identity

```text
Path:    crates/sandbox-daemon
Package: sandbox-daemon
Import:  sandbox_daemon
Binaries:
  sandbox-daemon
```

`sandbox-daemon` is the in-sandbox process. It owns process startup, daemon
transport, server lifecycle, and low-level helper subcommand adapters.

## Owns

- In-sandbox daemon process entrypoint.
- `serve`, `ns-runner`, and `ns-holder` subcommand routing.
- Unix/TCP listener lifecycle for daemon requests.
- Request framing at the server edge.
- Dispatching decoded `sandbox_protocol::Request` values to
  `sandbox-runtime`.
- Runtime wiring that builds `SandboxRuntimeOperations`.

## Must Not Own

- Sandbox creation/destruction.
- Manager operation catalog.
- CLI behavior.
- Concrete daemon operation implementation beyond dispatch wiring.
- Layerstack, overlay, command, or workspace primitives.

## Target Modules

```text
src/
  main.rs
  lib.rs
  config.rs
  wiring.rs
  serve.rs
  runner.rs
  holder.rs

  server/
    mod.rs
    runtime.rs
    lifecycle.rs
    connection.rs
    dispatch.rs
    error.rs
```

## Binary Interface

Target:

```text
sandbox-daemon serve --config-yaml PATH
sandbox-daemon ns-runner
sandbox-daemon ns-holder
```

## Protocol Contract

The daemon receives the same `Request` DTO as the manager:

```rust
pub struct Request {
    pub request_id: String,
    pub scope: OperationScope,
    pub op: String,
    pub args: serde_json::Value,
}
```

The daemon must only execute sandbox-scoped daemon operations. A request with
`OperationScope::System` is invalid at the daemon boundary. The sandbox id is
primarily for correlation and validation; the daemon process still executes
inside one sandbox selected by the manager.

## Dependency Rules

Allowed:

- `sandbox-protocol`
- `sandbox-runtime`
- `sandbox-runtime-config`
- `sandbox-runtime-namespace-process`
- async/network crates needed by the server

Forbidden:

- `sandbox-manager`
- `sandbox-gateway-cli`

The daemon can be launched by the manager, but it must not depend on the
manager.

## Verification

```sh
cargo fmt --check -p sandbox-daemon
cargo check -p sandbox-daemon --tests
cargo test -p sandbox-daemon
```
