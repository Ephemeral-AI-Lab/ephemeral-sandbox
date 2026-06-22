# Sandbox Runtime

Crate path: `crates/sandbox-runtime/operation`

Package: `sandbox-runtime`

`sandbox-runtime` is the daemon/runtime operation facade. It owns the runtime
operation catalog, request dispatch, typed argument parsing, response
projection, and orchestration across the runtime support packages.

## Boundary Rule

Runtime operation requests enter through `sandbox-daemon` and are dispatched to
`sandbox-runtime`.

```text
external protocol caller
  -> sandbox-daemon
    -> sandbox_runtime::dispatch_operation
      -> SandboxRuntimeOperations.command / CommandOperationService
        -> internal WorkspaceSessionService
        -> internal WorkspaceRemountService
          -> sandbox-runtime-workspace
          -> sandbox-runtime-layerstack
```

Manager operations stay in `sandbox-manager`. Low-level command, workspace,
namespace, layerstack, overlay, and config primitives stay in their separate
`sandbox-runtime-*` support packages.

## Runtime Operations

The external runtime operation surface is the `Command` family:

- `exec_command`
- `write_command_stdin`
- `read_command_lines`

Catalog help is rendered from protocol metadata:

```text
sandbox-cli runtime help
sandbox-cli runtime help exec_command
```

Runtime help usage and examples do not include `--sandbox-id`; sandbox
selection is contextual CLI configuration used before the request reaches the
runtime operation surface.

`CommandOperationService` owns command admission, active/completed command
tracking, transcript access, command launch, cancellation, and command
finalization state.

Command execution targets an existing workspace session. Command finalization
records the session command outcome and does not own layerstack publish
mechanics.

## Internal Runtime Lanes

`src/public/command` contains the external command operation lane.

`src/internal/workspace_session` tracks runtime workspace sessions over
`sandbox_runtime_workspace::WorkspaceRuntimeService`. It owns session create,
resolve, capture, destroy, and remount state transitions.

`src/internal/workspace_remount` coordinates running commands with workspace
remounts through narrow command and workspace-session ports.

## Runtime Support Packages

- `sandbox-runtime-command` owns process launch, PTY/transcript,
  process-group inspection, and command runtime primitives.
- `sandbox-runtime-workspace` owns workspace lifecycle, workspace handles,
  launch entries, capture, destroy, and remount primitives.
- `sandbox-runtime-namespace-process` owns the namespace holder and runner
  bodies, runner protocol DTOs, setns command execution, and in-namespace
  overlay/DNS helpers.
- `sandbox-runtime-layerstack` owns snapshot leases, publish, compaction,
  layer storage behavior, manifest schema, and CAS fixtures.
- `sandbox-runtime-overlay` owns low-level overlay mount, move, and unmount
  primitives shared by workspace and namespace process code.
- `sandbox-runtime-config` owns runtime YAML loading, merging, validation, and
  typed config schemas.

## Wiring

The daemon-facing aggregate is intentionally small:

```rust
pub struct SandboxRuntimeOperations {
    pub command: Arc<CommandOperationService>,
}
```

External dispatch code should receive `SandboxRuntimeOperations.command` or an
even narrower command wrapper. It should not receive workspace session or
workspace remount services as peer external operations.

Internal daemon setup may still construct all services:

```text
WorkspaceRuntimeService
  -> WorkspaceSessionService
  -> CommandOperationService
  -> WorkspaceRemountService
  -> SandboxRuntimeOperations { command }
```

## Verification

Use focused checks for the runtime facade and support packages:

```sh
cargo fmt --check -p sandbox-runtime
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime
```
