# daemon_operation

Crate path: `crates/daemon/daemon_operation`

`daemon_operation` is the daemon operation boundary. It sits above the low-level
daemon runtime crates and turns workspace/runtime primitives into daemon-level
operations. The crate is intentionally split into one external-facing lane for
agent command tool calls and internal lanes for workspace session state and
workspace remount orchestration.

## Boundary Rule

Only command operations are intended to be exposed to an external gateway or
agent tool-call surface.

```text
external gateway
  -> DaemonOperations.command / CommandOperationService
    -> internal WorkspaceSessionService
    -> internal WorkspaceRemountService
      -> workspace::WorkspaceRuntimeService
        -> layerstack::service
```

`workspace_session` and `workspace_remount` are daemon-internal orchestration
surfaces. They are still available as Rust modules for daemon wiring and tests,
but they should not become gateway/tool-call APIs.

## Public Command Lane

Module: `src/public/command`

The command lane owns the API that maps naturally to agent tool calls:

- `exec_command`
- `poll`
- `read_command_lines`
- `write_command_stdin`
- `cancel`

`CommandOperationService` owns command admission, active/completed command
tracking, transcript access, command launch, cancellation, and command
finalization state.

Command execution targets an existing workspace session. Command finalization
records the session command outcome and does not own layerstack publish
mechanics.

## Internal Workspace Session Lane

Module: `src/internal/workspace_session`

`WorkspaceSessionService` tracks daemon workspace sessions over
`workspace::WorkspaceRuntimeService`.

It owns:

- `create_workspace_session`
- `resolve_session`
- `capture_session_changes`
- `destroy_session`
- remount state transitions through dedicated workspace-session service impls

This layer is where command-facing workflows become workspace-runtime calls.
Workspace capture, destroy, and remount behavior lives here because that
behavior depends on session state, workspace handles, captured changes, and
layerstack roots.

## Internal Workspace Remount Lane

Module: `src/internal/workspace_remount`

`WorkspaceRemountService` is the coordinator between running commands and a
workspace remount. It depends on two narrow ports:

- `CommandRemountCoordinator`, implemented by the command service, quiesces or
  inspects active command process groups.
- `RemountWorkspaceSession`, implemented by workspace-session remount code,
  begins, applies, finishes, or blocks a remount.

`remount_workspace_session` returns `WorkspaceRemountOutcome`. This is an
operation result, not persistent state. It reports whether the remount happened,
why it was blocked if it did not, command inspection details, and the updated
workspace handler when remount succeeds.

## Lower-Level Runtime Crates

`daemon_operation` should not replace the lower-level daemon crates:

- `workspace` owns workspace runtime lifecycle, workspace handles, launch
  entries, capture, destroy, and remount primitives.
- `layerstack` owns snapshot lease, publish, compaction, and layer storage
  behavior.
- `command` owns process launch, PTY/transcript, process-group inspection, and
  command runtime primitives.

The operation crate composes those primitives. It should avoid duplicating them.

## Main Flows

### Session Command

```text
CommandOperationService::exec_command
  -> WorkspaceSessionService::resolve_session
  -> CommandLaunchDriver::spawn
  -> command remains attached to the existing workspace session
  -> finalization records SessionComplete
```

The workspace session remains alive after the command completes.


The command lane decides command status. The workspace session lane owns the
workspace lifecycle and publish/destroy details.

### Workspace Remount

```text
WorkspaceRemountService::remount_workspace_session
  -> RemountWorkspaceSession::begin_remount
  -> CommandRemountCoordinator::begin_workspace_remount_quiesce
  -> RemountWorkspaceSession::apply_and_finish_remount
  -> CommandRemountQuiesce::finish
  -> WorkspaceRemountOutcome
```

If command inspection or cancellation blocks the remount, the workspace session
is marked blocked and the outcome contains `remounted: false`.

## Wiring

The daemon-facing aggregate is intentionally small:

```rust
pub struct DaemonOperations {
    pub command: Arc<CommandOperationService>,
}
```

Gateway code should receive `DaemonOperations.command` or an even narrower
command wrapper. It should not receive workspace session or workspace remount
services as peer external operations.

Internal daemon setup may still construct all services:

```text
WorkspaceRuntimeService
  -> WorkspaceSessionService
  -> CommandOperationService
  -> WorkspaceRemountService
  -> DaemonOperations { command }
```

## Placement Rules

- Put gateway/tool-call behavior under `src/public/command`.
- Put command process quiesce and process-group state under
  `src/internal/workspace_remount/service/command`.
- Put workspace session lifecycle, capture, publish, destroy, and remount state
  under `src/internal/workspace_session`.
- Put cross-service remount coordination under
  `src/internal/workspace_remount`.
- Keep raw workspace lifecycle behavior in `workspace`.
- Keep snapshot and publish storage behavior in `layerstack`.

## Verification

Use focused checks for this crate:

```sh
cargo fmt --package daemon_operation --check
CARGO_TARGET_DIR=/tmp/daemon-operation-check cargo check -p daemon_operation --tests
CARGO_TARGET_DIR=/tmp/daemon-operation-check cargo test -p daemon_operation
```
