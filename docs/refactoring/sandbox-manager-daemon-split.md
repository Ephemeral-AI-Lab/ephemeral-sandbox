# Sandbox Manager / Daemon Split Refactor Spec

## Goal

Split the current daemon-centered tree into explicit sandbox control-plane and
data-plane components:

- `sandbox-manager` owns host-side sandbox lifecycle and daemon placement.
- `sandbox-daemon` owns in-sandbox operation execution.
- `sandbox-gateway-cli` owns the human-facing command line.
- `sandbox-protocol` owns the shared request, response, operation-spec, and
  catalog vocabulary used by all three.

This split is required because future agents will choose between two operation
surfaces: manager operations and daemon operations. That authority boundary must
be structural, not inferred from a loose operation family tag.

## Target Workspace Shape

Target shape:

```text
crates/
  sandbox-protocol/
  sandbox-manager/
  sandbox-gateway-cli/
  sandbox-daemon/

  sandbox-runtime/
    operation/          # package: sandbox-runtime
    command/
    workspace/
    namespace-process/
    layerstack/
    overlay/
    config/
```

The `sandbox-runtime/*` crates are runtime support crates for the in-sandbox
daemon. They are grouped together because they implement daemon behavior, but
only the `sandbox-runtime` package owns the daemon operation catalog.

Do not collapse the support crates into the `sandbox-runtime` facade package.
Keep them as separate packages under the shared folder so dependency direction
stays explicit.

Top-level crate specs live in:

```text
docs/refactoring/sandbox-protocol.md
docs/refactoring/sandbox-manager.md
docs/refactoring/sandbox-gateway-cli.md
docs/refactoring/sandbox-daemon.md
```

Grouped runtime crate specs live in:

```text
docs/refactoring/sandbox-runtime.md
```

The phase-by-phase implementation guide lives in:

```text
docs/refactoring/sandbox-implementation-guide.md
```

## Naming Decisions

| Current | Target | Notes |
|---|---|---|
| `daemon_rpc_protocol` | `sandbox-protocol` | Shared process contract, not daemon-owned behavior. |
| `daemon_operation` | `sandbox-runtime` | Concrete daemon/runtime operation catalog and dispatch. |
| `daemon` server crate | `sandbox-daemon` | In-sandbox RPC server and daemon binary entrypoint. |
| `eosd` binary crate | part of `sandbox-daemon` | Keep `eosd` as a compatibility bin during migration. |
| `command` | `sandbox-runtime-command` | Command process, PTY, transcript, and lifecycle primitives. |
| `workspace` | `sandbox-runtime-workspace` | Workspace lifecycle, handles, capture, destroy, remount. |
| `namespace-process` | `sandbox-runtime-namespace-process` | `ns-holder` and `ns-runner` subprocess bodies. |
| `layerstack` | `sandbox-runtime-layerstack` | Layer/CAS/manifest storage and publish mechanics. |
| `overlay` | `sandbox-runtime-overlay` | Low-level overlayfs mount primitives. |
| `config` | `sandbox-runtime-config` | Runtime config loading and schemas. |
| gateway concept | `sandbox-gateway-cli` | Crate is CLI-specific; installed binary can be `sandbox`. |

Package names should use hyphens. Rust crate imports use underscores:

```rust
use sandbox_protocol::{Request, Response};
use sandbox_runtime::operation_specs;
```

## Authority Boundary

The manager and daemon runtime expose separate operation catalogs.

```text
sandbox_manager::operation_specs()
sandbox_runtime::operation_specs()
```

Do not merge these into one catalog with a `Manager` or `Daemon` family. An
agent should select an authority first, then select an operation from that
authority's catalog.

```rust
pub enum OperationAuthority {
    SandboxManager,
    SandboxDaemon,
}

pub enum OperationTarget {
    Manager,
    Daemon { sandbox_id: SandboxId },
}
```

`OperationFamily` may still group operations inside a catalog for documentation
or manual rendering. It is not the routing authority.

## Shared Protocol Boundary

`sandbox-protocol` owns protocol-neutral types:

```text
request.rs          Request, OwnedRequest, args helpers
response.rs         Response, status/error helpers
framing.rs          JSON-line framing helpers
auth.rs             auth field constants
limits.rs           request size and timeout limits
operation_spec.rs   OperationSpec, ArgSpec, ArgKind, CliSpec
catalog.rs          OperationCatalog, OperationAuthority
manual.rs           manual/help rendering from OperationSpec
```

`sandbox-protocol` must not:

- Open sockets.
- Dispatch operations.
- Depend on `sandbox-manager`.
- Depend on `sandbox-daemon`.
- Know command, workspace, layerstack, or container runtime semantics.

`OperationEntry` stays out of `sandbox-protocol` because it binds a spec to an
implementation-specific dispatch function.

## Manager Responsibilities

`sandbox-manager` owns host-side lifecycle and daemon placement:

```text
sandbox-manager/
  src/lib.rs
  src/model.rs
  src/error.rs
  src/store.rs
  src/runtime.rs
  src/daemon_install.rs
  src/daemon_client.rs
  src/operation/
  src/server/
```

Manager entities:

```rust
pub struct SandboxId(String);

pub struct SandboxRecord {
    pub id: SandboxId,
    pub state: SandboxState,
    pub daemon: Option<SandboxDaemonEndpoint>,
}

pub enum SandboxState {
    Creating,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

pub struct SandboxDaemonEndpoint {
    pub socket_path: PathBuf,
    pub auth_token: Option<String>,
}
```

Manager operations:

```text
create_sandbox
destroy_sandbox
list_sandboxes
inspect_sandbox
start_sandbox_daemon
stop_sandbox_daemon
describe_manager_operations
describe_daemon_operations
invoke_sandbox_daemon
```

The manager may forward daemon requests, but it must not implement daemon
operation semantics. Forwarding is transport and lifecycle coordination only.

## Daemon Responsibilities

`sandbox-daemon` owns the in-sandbox server process:

```text
sandbox-daemon/
  Cargo.toml
  src/main.rs
  src/lib.rs
  src/server/
  src/runner.rs
  src/holder.rs
```

The current `eosd` role moves here. The target binary interface is:

```text
sandbox-daemon serve
sandbox-daemon ns-runner
sandbox-daemon ns-holder
```

During migration, keep a compatibility binary named `eosd` that dispatches to
the same implementation.

The `sandbox-runtime` package owns daemon operation semantics:

```text
sandbox-runtime/operation/
  Cargo.toml          # package: sandbox-runtime
  src/lib.rs
  src/operation.rs
  src/public/command/
  src/internal/workspace_session/
  src/internal/workspace_remount/
```

Daemon operations:

```text
exec_command
write_command_stdin
poll_command
read_command_lines
cancel_command
```

Daemon operations execute inside one sandbox. They must not create, destroy, or
select sandboxes.

## Gateway CLI Responsibilities

`sandbox-gateway-cli` owns the command line only:

```text
sandbox-gateway-cli/
  Cargo.toml
  src/main.rs
  src/config.rs
  src/client.rs
  src/manual.rs
  src/request_builder.rs
  src/output.rs
```

Package and binary:

```toml
[package]
name = "sandbox-gateway-cli"

[[bin]]
name = "sandbox"
```

The CLI sends requests to `sandbox-manager` by default. It should not require
direct daemon endpoint knowledge for normal use.

Example:

```text
sandbox create_sandbox
sandbox exec_command --sandbox sbox-1 --cmd "pwd"
sandbox poll_command --sandbox sbox-1 cmd-1
```

The CLI can render manuals by combining manager and daemon catalogs:

```text
Sandbox Manager Operations
  create_sandbox
  list_sandboxes
  destroy_sandbox

Sandbox Daemon Operations
  exec_command
  poll_command
  cancel_command
```

## Request Routing Model

The protocol request does not need to embed the sandbox target:

```json
{
  "id": "req-1",
  "op": "exec_command",
  "args": {
    "workspace_session_id": "ws-1",
    "cmd": "pwd"
  }
}
```

Targeting is a manager/gateway concern:

```rust
pub struct RoutedRequest {
    pub target: OperationTarget,
    pub request: sandbox_protocol::OwnedRequest,
}
```

For daemon operations, the gateway sends the target sandbox id to the manager.
The manager resolves `sandbox_id -> SandboxDaemonEndpoint` and forwards the
inner protocol request to the daemon.

## Implementation Order

### 0. Baseline And Guardrails

Capture the current state before moving crates:

```sh
cargo fmt --check -p daemon_rpc_protocol -p daemon_operation -p daemon -p eosd
cargo check -p daemon_rpc_protocol -p daemon_operation -p daemon -p eosd
cargo test -p daemon_rpc_protocol -p daemon_operation -p daemon
```

If the checkout is already dirty, record which failures are pre-existing before
starting the refactor.

### 1. Extract And Rename `sandbox-protocol`

Module order:

1. Move `crates/daemon/rpc_protocol` to `crates/sandbox-protocol`.
2. Rename package from `daemon_rpc_protocol` to `sandbox-protocol`.
3. Update imports from `daemon_rpc_protocol` to `sandbox_protocol`.
4. Move protocol-neutral spec types from daemon operation into
   `sandbox-protocol`:
   - `ArgKind`
   - `ArgCliSpec`
   - `ArgSpec`
   - `CliSpec`
   - `OperationSpec`
   - `OperationFamily` or a renamed `OperationGroup`
5. Add `OperationAuthority` and `OperationCatalog`.
6. Keep implementation-specific `OperationEntry` in operation crates.

Verification:

```sh
cargo fmt --check -p sandbox-protocol -p daemon_operation -p daemon
cargo check -p sandbox-protocol -p daemon_operation -p daemon
cargo test -p sandbox-protocol -p daemon_operation
```

### 2. Split Runtime Operation Catalog

Module order:

1. Move `crates/daemon/operation` to `crates/sandbox-runtime/operation`.
2. Rename `daemon_operation` package to `sandbox-runtime`.
3. Rename imports to `sandbox_runtime`.
4. Keep current module shape:
   - `public/command`
   - `internal/workspace_session`
   - `internal/workspace_remount`
5. Rename aggregate types only when the crate compiles:
   - `DaemonOperations` -> `SandboxDaemonOperations`
   - `OperationRequest` aliases continue to point at `sandbox_protocol`.
6. Export daemon operation catalog:
   - `sandbox_runtime::operation_specs()`
   - `sandbox_runtime::operation_catalog()`

Do not add manager operations to this crate.

Verification:

```sh
cargo fmt --check -p sandbox-runtime
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime
```

### 3. Rename Server/Binary Into `sandbox-daemon`

Module order:

1. Rename `daemon` server package to `sandbox-daemon`.
2. Move current server modules under `crates/sandbox-daemon/src/server` or keep
   the existing flat module shape if that is already the current tree.
3. Merge the `eosd` binary adapter into `sandbox-daemon/src/main.rs`.
4. Preserve the old `eosd` binary as a compatibility bin:

   ```toml
   [[bin]]
   name = "sandbox-daemon"
   path = "src/main.rs"

   [[bin]]
   name = "eosd"
   path = "src/main.rs"
   ```

5. Rename server types:
   - `DaemonServer` -> `SandboxDaemonServer`
   - `DaemonError` -> `SandboxDaemonError`
   - `ServerConfig` may stay generic if used only inside the daemon crate.
6. Keep subcommands:
   - `serve`
   - `ns-runner`
   - `ns-holder`
7. Keep `daemon` as a compatibility subcommand only if existing scripts require
   `eosd daemon` during transition.

Verification:

```sh
cargo fmt --check -p sandbox-daemon -p sandbox-runtime
cargo check -p sandbox-daemon -p sandbox-runtime
cargo test -p sandbox-daemon -p sandbox-runtime
```

### 4. Create `sandbox-manager` Model And Catalog

Module order:

1. `model.rs`: `SandboxId`, `SandboxRecord`, `SandboxState`,
   `SandboxDaemonEndpoint`.
2. `error.rs`: typed manager errors with stable protocol error kinds.
3. `store.rs`: in-memory sandbox registry.
4. `runtime.rs`: trait for sandbox runtime creation/destruction.
5. `daemon_install.rs`: trait for placing and starting `sandbox-daemon`.
6. `daemon_client.rs`: client for forwarding `sandbox-protocol` requests to a
   daemon endpoint.
7. `operation/specs.rs`: manager `OperationSpec` declarations.
8. `operation/dispatch.rs`: manager dispatch table.

First implementation should use test doubles or a local stub runtime. Do not
wire Docker/Firecracker before the manager operation contract is stable.

Verification:

```sh
cargo fmt --check -p sandbox-manager
cargo check -p sandbox-manager --tests
cargo test -p sandbox-manager
```

### 5. Add Manager Server

Module order:

1. `server/config.rs`: socket path, pid path, optional auth config.
2. `server/runtime.rs`: listener construction and shutdown token.
3. `server/connection.rs`: one framed request per connection.
4. `server/dispatch.rs`: dispatch manager operations.
5. `server/forward.rs`: forward daemon requests through
   `SandboxDaemonEndpoint`.

At this point, normal daemon operation flow should be:

```text
sandbox-gateway-cli
  -> sandbox-manager
    -> sandbox-daemon
```

Verification:

```sh
cargo fmt --check -p sandbox-manager
cargo check -p sandbox-manager --tests
cargo test -p sandbox-manager
```

### 6. Create `sandbox-gateway-cli`

Module order:

1. `config.rs`: manager socket/config discovery.
2. `client.rs`: sends `sandbox-protocol` requests to manager.
3. `manual.rs`: renders manager and daemon operation catalogs separately.
4. `request_builder.rs`: turns CLI argv into `Request`.
5. `output.rs`: stdout for data, stderr for errors.
6. `main.rs`: command dispatch and exit-code mapping.

CLI rules:

- The installed binary is `sandbox`.
- Errors go to stderr.
- Machine-readable responses go to stdout.
- Default path is gateway -> manager, not gateway -> daemon.
- Daemon commands require a `--sandbox` target unless a config default exists.

Verification:

```sh
cargo fmt --check -p sandbox-gateway-cli
cargo check -p sandbox-gateway-cli --tests
cargo test -p sandbox-gateway-cli
```

### 7. Agent-Facing Catalog Contract

Add a catalog endpoint or manager operation that returns both surfaces:

```text
describe_manager_operations
describe_daemon_operations
```

The returned data should preserve authority:

```json
{
  "manager": {
    "operations": ["create_sandbox", "list_sandboxes"]
  },
  "daemon": {
    "operations": ["exec_command", "poll_command", "cancel_command"]
  }
}
```

Agents should not infer authority from operation name prefixes alone. The
manager catalog and daemon catalog are separate tool spaces.

Verification:

```sh
cargo test -p sandbox-manager operation_catalog
cargo test -p sandbox-gateway-cli manual
```

### 8. Compatibility Cleanup

Only after all new names work:

1. Update README architecture.
2. Update packaging from `eosd` to `sandbox-daemon`.
3. Keep or remove `eosd` compatibility based on downstream scripts.
4. Remove old workspace dependency aliases.
5. Run stale-name scans:

   ```sh
   rg -n "daemon_rpc_protocol|daemon_operation|eosd daemon|crates/daemon/server"
   rg -n "poll\\b|cancel\\b" crates docs README.md
   ```

6. Run final focused checks:

   ```sh
   cargo fmt --check
   cargo check -p sandbox-protocol -p sandbox-manager -p sandbox-gateway-cli -p sandbox-daemon -p sandbox-runtime -p sandbox-runtime-command -p sandbox-runtime-workspace -p sandbox-runtime-namespace-process -p sandbox-runtime-layerstack -p sandbox-runtime-overlay -p sandbox-runtime-config
   cargo test -p sandbox-protocol -p sandbox-manager -p sandbox-gateway-cli -p sandbox-daemon -p sandbox-runtime -p sandbox-runtime-command -p sandbox-runtime-workspace -p sandbox-runtime-namespace-process -p sandbox-runtime-layerstack -p sandbox-runtime-overlay -p sandbox-runtime-config
   ```

## Non-Goals

- Do not rename all runtime support crates in the first split.
- Do not move command execution semantics into `sandbox-manager`.
- Do not make `sandbox-gateway-cli` own sandbox lifecycle.
- Do not make `OperationFamily` the manager-vs-daemon routing authority.
- Do not remove `command-request.json` until there is a replacement transport
  for the namespace runner side channel.

## Success Criteria

- `sandbox-manager` and `sandbox-daemon` have separate operation catalogs.
- `sandbox-runtime` owns the daemon catalog exposed by `sandbox-daemon`.
- `sandbox-gateway-cli` can generate/manual-render both catalogs separately.
- `sandbox-manager` can route a daemon operation to a selected sandbox daemon.
- `sandbox-daemon` can execute existing command operations unchanged in meaning.
- `sandbox-protocol` has no dependency on manager, daemon, command, workspace,
  layerstack, or namespace-process crates.
- Each implementation phase has a focused cargo check/test gate.
