# Sandbox Refactor Implementation Guide

This guide turns the sandbox manager/daemon split into reviewable phases. Each
phase should leave the workspace in a coherent state before the next phase
starts.

Reference specs:

```text
docs/refactoring/sandbox-manager-daemon-split.md
docs/refactoring/sandbox-protocol.md
docs/refactoring/sandbox-runtime.md
docs/refactoring/sandbox-daemon.md
docs/refactoring/sandbox-manager.md
docs/refactoring/sandbox-gateway-cli.md
```

## Package Order

```text
0. existing packages:
   daemon_rpc_protocol
   daemon_operation
   daemon
   eosd

1. sandbox-protocol
2. sandbox-runtime
3. sandbox-daemon
4. sandbox-manager core
5. sandbox-manager server and forwarding
6. sandbox-gateway-cli
7. catalog/manual contract
8. runtime support package rename wave
9. compatibility cleanup
```

Do not rename support packages while extracting the protocol, runtime facade,
daemon, manager, or gateway. Move them only in phase 8.

## Phase 0: Baseline

Goal:

- Capture current behavior and pre-existing failures before file moves.

Packages present:

```text
daemon_rpc_protocol
daemon_operation
daemon
eosd
command
workspace
namespace-process
layerstack
overlay
config
```

Resulting folder structure:

```text
crates/
  daemon/
    rpc_protocol/          # package: daemon_rpc_protocol
      Cargo.toml
      src/
      tests/
    operation/             # package: daemon_operation
      Cargo.toml
      src/public/
      src/internal/
      tests/
    server/                # package: daemon
      Cargo.toml
      src/
      tests/
    eosd/                  # package: eosd
      Cargo.toml
      src/
      tests/
    command/
    workspace/
    namespace-process/
    layerstack/
    overlay/
    config/
```

Verification:

```sh
cargo fmt --check -p daemon_rpc_protocol -p daemon_operation -p daemon -p eosd
cargo check -p daemon_rpc_protocol -p daemon_operation -p daemon -p eosd
cargo test -p daemon_rpc_protocol -p daemon_operation -p daemon
```

If any command fails, record the failure as pre-existing before changing
package names.

## Phase 1: Extract `sandbox-protocol`

Prompt:

```text
docs/refactoring/sandbox-phase-1-protocol-prompt.md
```

Goal:

- Move the shared protocol contract out of the daemon namespace.
- Move protocol-neutral operation metadata into the protocol crate.

Package moves:

```text
daemon_rpc_protocol -> sandbox-protocol
```

Implementation steps:

1. Move `crates/daemon/rpc_protocol` to `crates/sandbox-protocol`.
2. Rename package `daemon_rpc_protocol` to `sandbox-protocol`.
3. Rename imports from `daemon_rpc_protocol` to `sandbox_protocol`.
4. Move only protocol-neutral spec types from `daemon_operation`:
   - `ArgKind`
   - `ArgCliSpec`
   - `ArgSpec`
   - `CliSpec`
   - `OperationSpec`
   - `OperationFamily` or `OperationGroup`
5. Add protocol catalog types:
   - `OperationAuthority`
   - `OperationCatalog`
6. Keep implementation-bound dispatch entries in `daemon_operation`.

Resulting folder structure:

```text
crates/
  sandbox-protocol/        # package: sandbox-protocol
    Cargo.toml
    src/
      lib.rs
      request.rs
      response.rs
      framing.rs
      auth.rs
      limits.rs
      error_kind.rs
      operation_spec.rs
      catalog.rs
      manual.rs
    tests/

  daemon/
    operation/             # package: daemon_operation
      Cargo.toml
      src/                 # OperationEntry stays here
      tests/
    server/                # package: daemon
    eosd/                  # package: eosd
    command/
    workspace/
    namespace-process/
    layerstack/
    overlay/
    config/
```

Verification:

```sh
cargo fmt --check -p sandbox-protocol -p daemon_operation -p daemon
cargo check -p sandbox-protocol -p daemon_operation -p daemon
cargo test -p sandbox-protocol -p daemon_operation
```

Exit criteria:

- `sandbox-protocol` has no dependency on manager, daemon, runtime support, or
  operation dispatch crates.
- `daemon_operation` still owns `OperationEntry` and concrete dispatch.

## Phase 2: Extract `sandbox-runtime`

Prompt:

```text
docs/refactoring/sandbox-phase-2-runtime-prompt.md
```

Goal:

- Move daemon operation semantics into the runtime facade package.
- Preserve existing command operation behavior.

Package moves:

```text
daemon_operation -> sandbox-runtime
```

Implementation steps:

1. Move `crates/daemon/operation` to `crates/sandbox-runtime/operation`.
2. Rename package `daemon_operation` to `sandbox-runtime`.
3. Rename imports from `daemon_operation` to `sandbox_runtime`.
4. Keep the current operation module shape.
5. Rename aggregate types after the package compiles:
   - `DaemonOperations` -> `SandboxDaemonOperations`
6. Export:
   - `sandbox_runtime::operation_specs()`
   - `sandbox_runtime::operation_catalog()`

Resulting folder structure:

```text
crates/
  sandbox-protocol/

  sandbox-runtime/
    operation/             # package: sandbox-runtime
      Cargo.toml
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
      tests/

  daemon/
    server/                # package: daemon
    eosd/                  # package: eosd
    command/
    workspace/
    namespace-process/
    layerstack/
    overlay/
    config/
```

Verification:

```sh
cargo fmt --check -p sandbox-runtime
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime
```

Exit criteria:

- `sandbox-runtime` exposes only daemon/runtime operations.
- Manager operations are not added to this crate.
- Command operation names are `exec_command`, `poll_command`, and
  `cancel_command`, not `exec`, `poll`, or `cancel`.

## Phase 3: Create `sandbox-daemon`

Goal:

- Rename the in-sandbox server process and fold the `eosd` adapter into it.
- Keep `eosd` as a temporary compatibility binary.

Package moves:

```text
daemon -> sandbox-daemon
eosd   -> compatibility bin inside sandbox-daemon
```

Implementation steps:

1. Move `crates/daemon/server` to `crates/sandbox-daemon`.
2. Merge `crates/daemon/eosd/src` behavior into `sandbox-daemon/src/main.rs`.
3. Configure two binaries from the same package:

   ```toml
   [[bin]]
   name = "sandbox-daemon"
   path = "src/main.rs"

   [[bin]]
   name = "eosd"
   path = "src/main.rs"
   ```

4. Keep daemon subcommands:
   - `serve`
   - `ns-runner`
   - `ns-holder`
5. Keep `eosd daemon` as a compatibility alias only while scripts still need
   it.

Resulting folder structure:

```text
crates/
  sandbox-protocol/
  sandbox-runtime/
    operation/             # package: sandbox-runtime

  sandbox-daemon/          # package: sandbox-daemon
    Cargo.toml             # bins: sandbox-daemon, eosd
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
    tests/

  daemon/
    command/
    workspace/
    namespace-process/
    layerstack/
    overlay/
    config/
```

Verification:

```sh
cargo fmt --check -p sandbox-daemon -p sandbox-runtime
cargo check -p sandbox-daemon -p sandbox-runtime
cargo test -p sandbox-daemon -p sandbox-runtime
```

Exit criteria:

- `sandbox-daemon` depends on `sandbox-protocol` and `sandbox-runtime`.
- `sandbox-daemon` does not depend on `sandbox-manager` or
  `sandbox-gateway-cli`.
- Existing daemon command behavior is unchanged.

## Phase 4: Add `sandbox-manager` Core

Goal:

- Add the host-side control plane model and manager operation catalog.
- Avoid Docker, Firecracker, or production sandbox runtime wiring in this
  phase.

New package:

```text
sandbox-manager
```

Implementation steps:

1. Add manager domain model:
   - `SandboxId`
   - `SandboxRecord`
   - `SandboxState`
   - `SandboxDaemonEndpoint`
2. Add an in-memory store.
3. Add host runtime traits.
4. Add daemon install/start traits.
5. Add a daemon client abstraction for forwarding protocol requests.
6. Add manager operation specs and dispatch.

Resulting folder structure:

```text
crates/
  sandbox-protocol/
  sandbox-runtime/
    operation/
  sandbox-daemon/

  sandbox-manager/         # package: sandbox-manager
    Cargo.toml
    src/
      lib.rs
      model.rs
      error.rs
      store.rs
      runtime.rs
      daemon_install.rs
      daemon_client.rs
      operation/
        mod.rs
        specs.rs
        dispatch.rs
        impls/
          mod.rs
          create_sandbox.rs
          destroy_sandbox.rs
          list_sandboxes.rs
          inspect_sandbox.rs
          start_sandbox_daemon.rs
          stop_sandbox_daemon.rs
          describe_manager_operations.rs
          describe_daemon_operations.rs
          invoke_sandbox_daemon.rs
    tests/

  daemon/
    command/
    workspace/
    namespace-process/
    layerstack/
    overlay/
    config/
```

Verification:

```sh
cargo fmt --check -p sandbox-manager
cargo check -p sandbox-manager --tests
cargo test -p sandbox-manager
```

Exit criteria:

- Manager catalog and daemon catalog are separate.
- Manager may forward daemon requests but does not implement daemon operations.
- Tests can use stub runtimes and stub daemon endpoints.

## Phase 5: Add Manager Server And Forwarding

Goal:

- Make `sandbox-manager` a process endpoint.
- Route selected daemon operations to a sandbox daemon endpoint.

Package changed:

```text
sandbox-manager
```

Implementation steps:

1. Add server config.
2. Add listener lifecycle and shutdown handling.
3. Add one framed request per connection.
4. Dispatch manager operations locally.
5. Forward daemon operations through `SandboxDaemonEndpoint`.

Resulting folder structure:

```text
crates/
  sandbox-manager/
    Cargo.toml
    src/
      lib.rs
      model.rs
      error.rs
      store.rs
      runtime.rs
      daemon_install.rs
      daemon_client.rs
      operation/
      server/
        mod.rs
        config.rs
        lifecycle.rs
        connection.rs
        dispatch.rs
        forward.rs
    tests/
```

Request flow after this phase:

```text
client or test
  -> sandbox-manager
    -> sandbox-daemon
      -> sandbox-runtime
```

Verification:

```sh
cargo fmt --check -p sandbox-manager
cargo check -p sandbox-manager --tests
cargo test -p sandbox-manager
```

Exit criteria:

- Manager can resolve `SandboxId` to `SandboxDaemonEndpoint`.
- Manager forwarding is protocol forwarding, not a crate dependency on daemon
  runtime implementation.

## Phase 6: Add `sandbox-gateway-cli`

Goal:

- Add the human-facing command line.
- Keep the CLI as a protocol client, not a hidden manager.

New package:

```text
sandbox-gateway-cli
```

Implementation steps:

1. Add manager socket/config discovery.
2. Add manager client connection.
3. Add request construction from CLI argv and `OperationSpec`.
4. Add manual/help rendering from manager and daemon catalogs.
5. Add stdout/stderr and exit-code behavior.
6. Add the installed binary name `sandbox`.

Resulting folder structure:

```text
crates/
  sandbox-gateway-cli/     # package: sandbox-gateway-cli
    Cargo.toml             # bin: sandbox
    src/
      main.rs
      config.rs
      client.rs
      manual.rs
      request_builder.rs
      output.rs
    tests/

  sandbox-protocol/
  sandbox-manager/
  sandbox-daemon/
  sandbox-runtime/
    operation/
```

Verification:

```sh
cargo fmt --check -p sandbox-gateway-cli
cargo check -p sandbox-gateway-cli --tests
cargo test -p sandbox-gateway-cli
```

Exit criteria:

- Default route is gateway -> manager.
- Daemon operations require `--sandbox SANDBOX_ID` unless config supplies a
  default.
- Errors go to stderr and machine-readable responses go to stdout.

## Phase 7: Stabilize Catalog And Manual Contract

Goal:

- Make manager and daemon catalogs discoverable by agents and CLI help.

Packages changed:

```text
sandbox-protocol
sandbox-manager
sandbox-gateway-cli
```

Implementation steps:

1. Stabilize `OperationCatalog` and `OperationAuthority`.
2. Ensure manager catalog returns manager operations only.
3. Ensure daemon catalog returns daemon operations only.
4. Add or verify:
   - `describe_manager_operations`
   - `describe_daemon_operations`
5. Render CLI/manual output from `OperationSpec`, not duplicated strings.

Resulting folder structure:

```text
crates/
  sandbox-protocol/
    src/
      operation_spec.rs
      catalog.rs
      manual.rs

  sandbox-manager/
    src/
      operation/
        specs.rs
        dispatch.rs
        impls/
          describe_manager_operations.rs
          describe_daemon_operations.rs

  sandbox-gateway-cli/
    src/
      manual.rs
      request_builder.rs
```

Verification:

```sh
cargo test -p sandbox-manager operation_catalog
cargo test -p sandbox-gateway-cli manual
```

Exit criteria:

- Agents choose authority first, then operation.
- `OperationFamily` or `OperationGroup` is documentation grouping only, not the
  manager-vs-daemon routing authority.

## Phase 8: Rename Runtime Support Packages

Goal:

- Move runtime support packages from `crates/daemon/*` into
  `crates/sandbox-runtime/*`.
- Keep support packages separate from the `sandbox-runtime` facade package.

Package moves:

```text
command           -> sandbox-runtime-command
workspace         -> sandbox-runtime-workspace
namespace-process -> sandbox-runtime-namespace-process
layerstack        -> sandbox-runtime-layerstack
overlay           -> sandbox-runtime-overlay
config            -> sandbox-runtime-config
```

Implementation steps:

1. Move each package one at a time.
2. Update package names and imports.
3. Preserve existing public behavior.
4. Keep `command-request.json` until an explicit replacement transport exists.
5. Verify each moved package before moving the next one.

Resulting folder structure:

```text
crates/
  sandbox-protocol/
  sandbox-runtime/
    operation/             # package: sandbox-runtime
    command/               # package: sandbox-runtime-command
      Cargo.toml
      src/
      tests/
    workspace/             # package: sandbox-runtime-workspace
      Cargo.toml
      src/
      tests/
    namespace-process/     # package: sandbox-runtime-namespace-process
      Cargo.toml
      src/
      tests/
    layerstack/            # package: sandbox-runtime-layerstack
      Cargo.toml
      src/
      tests/
    overlay/               # package: sandbox-runtime-overlay
      Cargo.toml
      src/
      tests/
    config/                # package: sandbox-runtime-config
      Cargo.toml
      src/
      tests/

  sandbox-daemon/
  sandbox-manager/
  sandbox-gateway-cli/
```

Verification:

```sh
cargo fmt --check -p sandbox-runtime-command
cargo check -p sandbox-runtime-command --tests
cargo test -p sandbox-runtime-command

cargo fmt --check -p sandbox-runtime-workspace
cargo check -p sandbox-runtime-workspace --tests
cargo test -p sandbox-runtime-workspace

cargo fmt --check -p sandbox-runtime-namespace-process
cargo check -p sandbox-runtime-namespace-process --tests
cargo test -p sandbox-runtime-namespace-process

cargo fmt --check -p sandbox-runtime-layerstack
cargo check -p sandbox-runtime-layerstack --tests
cargo test -p sandbox-runtime-layerstack

cargo fmt --check -p sandbox-runtime-overlay
cargo check -p sandbox-runtime-overlay --tests
cargo test -p sandbox-runtime-overlay

cargo fmt --check -p sandbox-runtime-config
cargo check -p sandbox-runtime-config --tests
cargo test -p sandbox-runtime-config
```

Exit criteria:

- `sandbox-runtime-command` owns command process, PTY, transcript, and command
  request construction.
- `sandbox-runtime-workspace` owns workspace lifecycle.
- `sandbox-runtime-namespace-process` owns `ns-holder` and `ns-runner` bodies.
- `sandbox-runtime-overlay` remains a shared low-level mount primitive crate.
- `sandbox-runtime-layerstack` and `sandbox-runtime-config` do not depend on
  higher runtime implementation crates.

## Phase 9: Compatibility Cleanup

Goal:

- Remove stale names and update packaging after the new shape works.

Implementation steps:

1. Update README and architecture docs.
2. Update packaging from `eosd` to `sandbox-daemon`.
3. Decide whether to keep the `eosd` compatibility binary.
4. Remove old workspace dependency aliases.
5. Run stale-name scans.

Final folder structure:

```text
crates/
  sandbox-protocol/
  sandbox-runtime/
    operation/             # package: sandbox-runtime
    command/               # package: sandbox-runtime-command
    workspace/             # package: sandbox-runtime-workspace
    namespace-process/     # package: sandbox-runtime-namespace-process
    layerstack/            # package: sandbox-runtime-layerstack
    overlay/               # package: sandbox-runtime-overlay
    config/                # package: sandbox-runtime-config
  sandbox-daemon/
  sandbox-manager/
  sandbox-gateway-cli/
```

Stale-name scans:

```sh
rg -n "daemon_rpc_protocol|daemon_operation|crates/daemon/server" crates README.md docs --glob '!docs/refactoring/**'
rg -n "sandbox-runtime[-_]operation|sandbox_runtime[_]operation" crates README.md docs --glob '!docs/refactoring/**'
rg -n "poll\\b|cancel\\b" crates README.md docs --glob '!docs/refactoring/**'
```

Final verification:

```sh
cargo fmt --check
cargo check -p sandbox-protocol -p sandbox-runtime -p sandbox-daemon -p sandbox-manager -p sandbox-gateway-cli -p sandbox-runtime-command -p sandbox-runtime-workspace -p sandbox-runtime-namespace-process -p sandbox-runtime-layerstack -p sandbox-runtime-overlay -p sandbox-runtime-config
cargo test -p sandbox-protocol -p sandbox-runtime -p sandbox-daemon -p sandbox-manager -p sandbox-gateway-cli -p sandbox-runtime-command -p sandbox-runtime-workspace -p sandbox-runtime-namespace-process -p sandbox-runtime-layerstack -p sandbox-runtime-overlay -p sandbox-runtime-config
```

Exit criteria:

- `crates/daemon/` is gone or contains only explicitly documented temporary
  compatibility material.
- Old runtime-operation package/import names do not appear in active code or
  non-refactoring docs.
- `poll` and `cancel` do not remain as operation or file names; use
  `poll_command` and `cancel_command`.
- The CLI, manager, daemon, and runtime can be built and tested by package.
