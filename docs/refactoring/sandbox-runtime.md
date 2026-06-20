# Sandbox Runtime Crate Specs

This document groups only the `sandbox-runtime/*` crate specs for the sandbox
manager/daemon split. Keep these specs together so the runtime support crate
boundaries can be reviewed as one system.

Top-level crate specs live in:

```text
docs/refactoring/sandbox-protocol.md
docs/refactoring/sandbox-manager.md
docs/refactoring/sandbox-gateway-cli.md
docs/refactoring/sandbox-daemon.md
```

## Target Tree

```text
crates/
  sandbox-runtime/
    operation/          # package: sandbox-runtime
    command/
    workspace/
    namespace-process/
    layerstack/
    overlay/
    config/
```

Each `sandbox-runtime/*` entry is a separate Cargo package under one folder.
The `operation/` package is named `sandbox-runtime` because it is the daemon
runtime facade and owns the daemon operation catalog. Do not collapse the
support packages into that facade crate.

## Dependency Direction

```text
sandbox-gateway-cli
  -> sandbox-protocol
  -> sandbox-manager endpoint over protocol

sandbox-manager
  -> sandbox-protocol
  -> sandbox-daemon endpoint over protocol

sandbox-daemon
  -> sandbox-protocol
  -> sandbox-runtime
  -> sandbox-runtime-config

sandbox-runtime
  -> sandbox-protocol
  -> sandbox-runtime-command
  -> sandbox-runtime-workspace

sandbox-runtime-command
  -> sandbox-runtime-workspace
  -> sandbox-runtime-namespace-process

sandbox-runtime-workspace
  -> sandbox-runtime-layerstack
  -> sandbox-runtime-namespace-process

sandbox-runtime-namespace-process
  -> sandbox-runtime-overlay
  -> sandbox-runtime-config

sandbox-runtime-layerstack
  -> no sibling runtime package

sandbox-runtime-overlay
  -> no sibling runtime package

sandbox-runtime-config
  -> no sibling runtime package
```

`sandbox-manager` and `sandbox-runtime` own separate operation catalogs. Agents
choose the catalog first, then choose an operation.

## sandbox-runtime

```text
Path:    crates/sandbox-runtime/operation
Package: sandbox-runtime
Import:  sandbox_runtime
```

Owns:

- Daemon/runtime operation specs and catalog.
- Daemon/runtime operation dispatch table.
- Command operation service.
- Internal workspace session orchestration.
- Internal workspace remount orchestration.
- Request argument parsing into typed operation inputs.
- Response projection from runtime outputs into protocol responses.

Must not own:

- Manager operations.
- Sandbox creation/destruction.
- Socket listeners.
- CLI parsing.
- Low-level command PTY/process implementation.
- Low-level layerstack/overlay implementation.

Target modules:

```text
src/
  lib.rs
  operation.rs

  public/
    mod.rs
    command/
      mod.rs
      service.rs
      service/
        impls/
          mod.rs
          exec_command.rs
          write_command_stdin.rs
          poll_command.rs
          read_command_lines.rs
          cancel_command.rs

  internal/
    mod.rs
    services.rs
    workspace_session/
    workspace_remount/
```

Daemon/runtime operations:

```text
exec_command
write_command_stdin
poll_command
read_command_lines
cancel_command
```

Dependencies:

- Allowed: `sandbox-protocol`, `sandbox-runtime-command`,
  `sandbox-runtime-workspace`, `serde_json`, `thiserror`.
- Forbidden: `sandbox-manager`, `sandbox-gateway-cli`, `sandbox-daemon` server
  internals.

Migration source:

```text
crates/daemon/operation
```

Verification:

```sh
cargo fmt --check -p sandbox-runtime
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime
```

## sandbox-runtime-command

```text
Path:    crates/sandbox-runtime/command
Package: sandbox-runtime-command
Import:  sandbox_runtime_command
```

Owns:

- Command process spec and process handle.
- PTY setup and IO loops.
- Process group inspection and cancellation primitives.
- Transcript storage and transcript row projection.
- Yield/wait loop primitives.
- Construction of namespace runner command requests from validated workspace
  entries.
- Command runtime config.

Must not own:

- External operation specs.
- Operation dispatch.
- Workspace lifecycle.
- Layerstack publish/capture behavior.
- Sandbox lifecycle.
- CLI or manager behavior.

Target modules:

```text
src/
  lib.rs
  config.rs
  contract.rs
  process.rs
  process_group.rs
  pty.rs
  transcript.rs
  transcript_rows.rs
  yield_wait_loop.rs
```

Dependencies:

- Allowed: `sandbox-runtime-namespace-process` for namespace runner protocol
  types, `sandbox-runtime-workspace` only for validated workspace entry types
  needed to launch a command, OS/process/PTY crates, `serde`, `serde_json`.
- Forbidden: `sandbox-protocol`, `sandbox-manager`, `sandbox-gateway-cli`,
  `sandbox-daemon`, `sandbox-runtime-layerstack`.

Migration source:

```text
crates/daemon/command
```

Keep `command-request.json` until a replacement such as `--request-fd` exists.

Verification:

```sh
cargo fmt --check -p sandbox-runtime-command
cargo check -p sandbox-runtime-command --tests
cargo test -p sandbox-runtime-command
```

## sandbox-runtime-workspace

```text
Path:    crates/sandbox-runtime/workspace
Package: sandbox-runtime-workspace
Import:  sandbox_runtime_workspace
```

Owns:

- Workspace mode/session lifecycle primitives.
- Workspace handles and validated workspace entries.
- Namespace holder lifecycle coordination.
- Workspace create, destroy, touch, capture, and remount primitives.
- Overlay directory planning at the workspace level.
- Layerstack root association.
- Workspace runtime service and test hooks.

Must not own:

- Command process state.
- Command PTY/transcript behavior.
- Operation dispatch.
- Manager sandbox lifecycle.
- Low-level overlayfs syscalls.
- Layerstack storage internals.

Target modules:

```text
src/
  lib.rs
  error.rs
  model.rs
  service.rs
  service/
    hooks.rs
    support.rs

  lifecycle/
    mod.rs
    create.rs
    destroy.rs
    leases.rs
    persistence.rs
    remount/

  namespace/
    mod.rs
    cgroup.rs
    fds.rs
    holder.rs
    setns_runner.rs

  overlay/
    mod.rs
    capture.rs
    dirs.rs
    tree.rs

  profile/
    mod.rs
    handle.rs
    manager.rs

  isolated_setup/
    mod.rs
    dns.rs
    rtnl.rs
```

Dependencies:

- Allowed: `sandbox-runtime-layerstack`, `sandbox-runtime-namespace-process`,
  `sandbox-runtime-overlay` only through low-level mount interfaces where
  workspace lifecycle needs them, OS namespace/network crates.
- Forbidden: `sandbox-protocol`, `sandbox-manager`, `sandbox-gateway-cli`,
  `sandbox-daemon`, `sandbox-runtime-command`.

Migration source:

```text
crates/daemon/workspace
```

Verification:

```sh
cargo fmt --check -p sandbox-runtime-workspace
cargo check -p sandbox-runtime-workspace --tests
cargo test -p sandbox-runtime-workspace
```

## sandbox-runtime-namespace-process

```text
Path:    crates/sandbox-runtime/namespace-process
Package: sandbox-runtime-namespace-process
Import:  sandbox_runtime_namespace_process
```

Owns:

- `ns-holder` body.
- `ns-runner` body.
- Namespace runner protocol DTOs.
- Setns command execution.
- Setns overlay mount/remount/probe behavior.
- DNS setup performed inside the target namespace.
- Exit-code preserving holder errors.

Must not own:

- Daemon transport.
- Manager lifecycle.
- CLI parsing outside the thin helper adapter.
- Workspace session registry.
- Command operation dispatch.
- Layerstack storage.

Target modules:

```text
src/
  lib.rs

  holder/
    mod.rs
    namespace.rs
    network.rs

  runner/
    mod.rs
    protocol.rs
    setns.rs
    shell_exec.rs
```

Dependencies:

- Allowed: `sandbox-runtime-config`, `sandbox-runtime-overlay`, OS
  namespace/process crates, `serde`, `serde_json`.
- Forbidden: `sandbox-protocol`, `sandbox-manager`, `sandbox-gateway-cli`,
  `sandbox-daemon`, `sandbox-runtime`, `sandbox-runtime-workspace`.

Migration source:

```text
crates/daemon/namespace-process
```

Verification:

```sh
cargo fmt --check -p sandbox-runtime-namespace-process
cargo check -p sandbox-runtime-namespace-process --tests
cargo test -p sandbox-runtime-namespace-process
```

## sandbox-runtime-layerstack

```text
Path:    crates/sandbox-runtime/layerstack
Package: sandbox-runtime-layerstack
Import:  sandbox_runtime_layerstack
```

Owns:

- Layer and manifest models.
- Content hashing and storage.
- Workspace base layer construction.
- Commit route and transaction logic.
- Lease and cache behavior.
- Storage locks and whiteout handling.
- Layerstack test fixtures.

Must not own:

- Command execution.
- Workspace session registry.
- Operation dispatch.
- Daemon transport.
- Manager lifecycle.
- Overlayfs mount syscalls.

Target modules:

```text
src/
  lib.rs
  error.rs
  service.rs

  commit/
  model/
  service/
  stack/
  storage/
  workspace_base/
```

Dependencies:

- Allowed: hashing, filesystem, ignore/walk, serialization, and error crates.
- Forbidden: `sandbox-protocol`, `sandbox-manager`, `sandbox-gateway-cli`,
  `sandbox-daemon`, `sandbox-runtime-command`, `sandbox-runtime-workspace`,
  `sandbox-runtime-overlay`.

Migration source:

```text
crates/daemon/layerstack
```

Verification:

```sh
cargo fmt --check -p sandbox-runtime-layerstack
cargo check -p sandbox-runtime-layerstack --tests
cargo test -p sandbox-runtime-layerstack
```

## sandbox-runtime-overlay

```text
Path:    crates/sandbox-runtime/overlay
Package: sandbox-runtime-overlay
Import:  sandbox_runtime_overlay
```

Owns:

- Overlay mount request types.
- Overlay mount handles.
- Overlay mount, move, and unmount operations.
- Kernel mount adapter code.
- Test-only root override feature for overlay-backed tests.

Must not own:

- Workspace lifecycle.
- Namespace process orchestration.
- Layerstack storage behavior.
- Operation dispatch.
- Manager lifecycle.
- CLI or daemon transport.

Target modules:

```text
src/
  lib.rs
  kernel_mount.rs
```

Dependencies:

- Allowed: `rustix`, `thiserror`.
- Forbidden: `sandbox-protocol`, `sandbox-manager`, `sandbox-gateway-cli`,
  `sandbox-daemon`, `sandbox-runtime`, `sandbox-runtime-command`,
  `sandbox-runtime-workspace`, `sandbox-runtime-layerstack`.

Migration source:

```text
crates/daemon/overlay
```

Do not move overlay wholly under workspace; namespace-process also needs it for
mount/remount syscalls inside target namespaces.

Verification:

```sh
cargo fmt --check -p sandbox-runtime-overlay
cargo check -p sandbox-runtime-overlay --tests
cargo test -p sandbox-runtime-overlay
```

## sandbox-runtime-config

```text
Path:    crates/sandbox-runtime/config
Package: sandbox-runtime-config
Import:  sandbox_runtime_config
```

Owns:

- YAML parser adapter.
- Config document loading and merging.
- Production config path resolution.
- Test override path policy.
- Typed runtime config schemas.
- Validation helpers for config sections.

Must not own:

- Manager lifecycle state.
- Daemon transport.
- Command execution.
- Workspace lifecycle.
- Layerstack behavior.
- Overlayfs mount behavior.

Target modules:

```text
src/
  lib.rs
  document.rs
  error.rs
  merge.rs
  paths.rs
  yaml.rs

  configs/
    mod.rs
    daemon.rs
    isolated.rs
    runner.rs
    validate.rs
```

Dependencies:

- Allowed: `serde`, `serde_path_to_error`, YAML parser crate, `thiserror`.
- Forbidden: `sandbox-protocol`, `sandbox-manager`, `sandbox-gateway-cli`,
  `sandbox-daemon`, all runtime implementation crates.

Migration source:

```text
crates/daemon/config
```

Verification:

```sh
cargo fmt --check -p sandbox-runtime-config
cargo check -p sandbox-runtime-config --tests
cargo test -p sandbox-runtime-config
```
