# sandbox-daemon Crate Spec

## Identity

```text
Path:    crates/sandbox-daemon
Package: sandbox-daemon
Import:  sandbox_daemon
Binaries:
  sandbox-daemon
  eosd              # temporary compatibility binary
```

`sandbox-daemon` is the in-sandbox process. It owns process startup, daemon
transport, server lifecycle, and low-level helper subcommand adapters.

## Owns

- In-sandbox daemon process entrypoint.
- `serve`, `ns-runner`, and `ns-holder` subcommand routing.
- Unix/TCP listener lifecycle for daemon requests.
- Request framing at the server edge.
- Dispatching decoded requests to `sandbox-runtime`.
- Runtime wiring that builds `SandboxDaemonOperations`.
- Temporary `eosd` compatibility entrypoint.

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
sandbox-daemon serve
sandbox-daemon ns-runner
sandbox-daemon ns-holder
```

Temporary compatibility:

```text
eosd daemon
eosd ns-runner
eosd ns-holder
```

`eosd daemon` may remain as a compatibility alias for `sandbox-daemon serve`
until packaging and scripts have moved.

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

## Migration Source

Move from:

```text
crates/daemon/server
crates/daemon/eosd
```

The current `eosd` adapter becomes the binary entrypoint for `sandbox-daemon`.

## Verification

```sh
cargo fmt --check -p sandbox-daemon
cargo check -p sandbox-daemon --tests
cargo test -p sandbox-daemon
```
