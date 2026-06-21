# Sandbox CLI Help And Operation Metadata Spec

## Goal

Replace the flat `sandbox-cli manual` experience with scoped catalog help:

```text
sandbox-cli manager help
sandbox-cli manager help create_sandbox

sandbox-cli runtime help
sandbox-cli runtime help exec_command
```

The help text must be generated from protocol-owned operation metadata, not
duplicated by the CLI renderer. The operation metadata should become rich
enough to render grouped overviews, detailed operation pages, examples, related
operations, and targeted search/suggestion output.

This spec intentionally revises the older phase-7 flat-catalog rule that said
operation specs should not contain grouping metadata.

## User-Facing Rules

- Use `help`, not `manual`.
- Do not keep a top-level `sandbox-cli manual` command.
- Do not add a compatibility alias unless a later compatibility decision
  explicitly asks for it.
- Keep parser help and catalog help distinct:
  - `sandbox-cli manager --help` is Clap/parser syntax help.
  - `sandbox-cli manager help` is catalog help.
  - `sandbox-cli runtime --help` is Clap/parser syntax help.
  - `sandbox-cli runtime help` is catalog help.
- `sandbox-cli manager help` renders only manager operation families.
- `sandbox-cli runtime help` renders only runtime operation families.
- `sandbox-cli manager help OPERATION` renders one detailed manager operation
  page.
- `sandbox-cli runtime help OPERATION` renders one detailed runtime operation
  page.
- Runtime help command examples and rendered usage should not show
  `--sandbox-id`. Help is scoped to the runtime execution space; sandbox
  selection is contextual config.
- Runtime help still needs a runtime catalog. For the first implementation,
  resolve it from the configured/default sandbox, the same way runtime catalog
  discovery works today. If no default sandbox is available, fail with a direct
  usage error:

```text
runtime help requires a default sandbox
```

## Operation Families

Families are first-class catalog metadata. A family describes a coherent group
of operations inside one execution space.

Current manager families:

```text
Sandbox Lifecycle
  Manage host-side sandbox records and their lifecycle.

Sandbox Daemon Control
  Start and stop sandbox daemons.

Catalog Discovery
  Inspect supported operation catalogs.
```

Current runtime families:

```text
Command
  Run, interact with, inspect, and cancel commands inside the active sandbox runtime.
```

Future runtime families should be added beside `Command`, not by splitting the
current command family prematurely:

```text
Cgroup Monitor
File
Plugin
```

## Source Layout By Family

The catalog family model should be visible in the operation implementation tree.
Do not leave manager operations as one flat `impls/` directory after this
change.

Target manager layout:

```text
crates/sandbox-manager/src/operation/impls/
  mod.rs
  sandbox_lifecycle/
    mod.rs
    create_sandbox.rs
    destroy_sandbox.rs
    list_sandboxes.rs
    inspect_sandbox.rs
  daemon_control/
    mod.rs
    start_sandbox_daemon.rs
    stop_sandbox_daemon.rs
  catalog_discovery/
    mod.rs
    describe_manager_operations.rs
    describe_daemon_operations.rs
```

The folder names should map directly to operation family ids:

```text
sandbox_lifecycle  -> Sandbox Lifecycle
daemon_control     -> Sandbox Daemon Control
catalog_discovery  -> Catalog Discovery
```

`crates/sandbox-manager/src/operation/impls/mod.rs` should become a thin family
aggregator. Each family module owns the operation implementation modules in that
family and exposes that family's entries/spec references to the manager
operation dispatcher/catalog builder.

The runtime side should keep its current command lane shape for now:

```text
crates/sandbox-runtime/operation/src/public/command/
```

When future runtime families such as `cgroup_monitor`, `file`, or `plugin` are
added, they should be added as peer public runtime lanes rather than mixed into
`public/command`.

## Metadata Model

Add protocol-owned family and detail metadata. `sandbox-protocol` owns the DTOs
and catalog JSON conversion/parsing helpers. It must not own concrete manager
or runtime operation lists.

Target Rust shape:

```rust
pub struct OperationFamilySpec {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
}

pub struct OperationSpec {
    pub name: &'static str,
    pub family: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub args: &'static [ArgSpec],
    pub cli: Option<CliSpec>,
    pub related: &'static [&'static str],
}

pub struct OperationCatalog {
    pub operation_execution_space: OperationExecutionSpace,
    pub families: &'static [&'static OperationFamilySpec],
    pub operations: &'static [&'static OperationSpec],
}
```

Document types should mirror the same data with owned strings:

```rust
pub struct OperationFamilyDocument {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub description: String,
}

pub struct OperationSpecDocument {
    pub name: String,
    pub family: String,
    pub summary: String,
    pub description: String,
    pub args: Vec<ArgSpecDocument>,
    pub cli: Option<CliSpecDocument>,
    pub related: Vec<String>,
}
```

Catalog JSON shape:

```json
{
  "operation_execution_space": "runtime",
  "families": [
    {
      "id": "command",
      "title": "Command",
      "summary": "Run, interact with, inspect, and cancel commands.",
      "description": "Run, interact with, inspect, and cancel commands inside the active sandbox runtime."
    }
  ],
  "operations": [
    {
      "name": "exec_command",
      "family": "command",
      "summary": "Start a command in a workspace.",
      "description": "Start a shell command inside an existing workspace session.",
      "args": [],
      "cli": null,
      "related": ["poll_command", "write_command_stdin", "read_command_lines", "cancel_command"]
    }
  ]
}
```

Catalog decoding must reject:

- duplicate family ids;
- an operation whose `family` does not exist in the catalog;
- duplicate operation names;
- a related operation name that does not exist in the same catalog.

## CLI Shape

The current dynamic operation parser should reserve `help` inside both
execution spaces before trying to dispatch catalog operations.

Target command grammar:

```text
sandbox-cli manager help [OPERATION]
sandbox-cli manager OPERATION [ARGS...]

sandbox-cli runtime help [OPERATION]
sandbox-cli runtime OPERATION [ARGS...]
```

Implementation detail:

- `manager help` loads `describe_manager_operations`.
- `runtime help` loads `describe_daemon_operations` through the default sandbox.
- operation dispatch still validates that the selected catalog execution space
  matches the scoped command.
- `help` is reserved and cannot be used as an operation name.

## Help Rendering

Move the renderer from `manual` vocabulary to `help` vocabulary.

Target protocol functions:

```rust
pub fn render_catalog_help(catalog: &OperationCatalogDocument) -> String;
pub fn render_operation_help(
    catalog: &OperationCatalogDocument,
    operation: &str,
) -> Result<String, HelpRenderError>;
pub fn search_operation_help(
    catalog: &OperationCatalogDocument,
    query: &str,
) -> Vec<OperationSearchResult>;
```

The renderer should group operations by the family order declared in
`catalog.families`. Operations inside a family should use catalog order.

Overview format:

```text
Sandbox Runtime Help

Command
  Run, interact with, inspect, and cancel commands inside the active sandbox runtime.

  exec_command
    Start a command in a workspace.

  write_command_stdin
    Write text to a running command stdin.

  poll_command
    Poll a command status and recent output.

  read_command_lines
    Read a retained command transcript window by line offset.

  cancel_command
    Cancel a running command.

Use:
  sandbox-cli runtime help OPERATION
```

Detailed operation format:

```text
exec_command

Family
  Command

Description
  Start a shell command inside an existing workspace session. If the command is
  still running after the initial wait, the response includes a command_session_id
  that can be used with poll_command, write_command_stdin, read_command_lines,
  or cancel_command.

Usage
  sandbox-cli runtime exec_command --workspace-session-id ID COMMAND

Arguments
  --workspace-session-id string required
    Workspace session id to run inside.

  COMMAND string required
    Shell command text.

  --timeout-seconds float optional
    Command timeout in seconds.

  --yield-time-ms integer optional
    Initial output wait in milliseconds.

Examples
  sandbox-cli runtime exec_command --workspace-session-id ws-1 pwd
  sandbox-cli runtime exec_command --workspace-session-id ws-1 --yield-time-ms 0 "sleep 30"

Related Operations
  poll_command
  write_command_stdin
  read_command_lines
  cancel_command
```

Manager overview example:

```text
Sandbox Manager Help

Sandbox Lifecycle
  Manage host-side sandbox records and their lifecycle.

  create_sandbox
    Create a host-side sandbox record and runtime sandbox.

  destroy_sandbox
    Destroy a host-side sandbox and remove it from the registry.

  list_sandboxes
    List sandbox records known to the manager.

  inspect_sandbox
    Inspect one sandbox record.

Sandbox Daemon Control
  Start and stop sandbox daemons.

  start_sandbox_daemon
    Install and start the selected sandbox daemon.

  stop_sandbox_daemon
    Stop the selected sandbox daemon and clear its endpoint.

Catalog Discovery
  Inspect supported operation catalogs.

  describe_manager_operations
    Describe manager operation specs.

  describe_daemon_operations
    Describe runtime operation specs for a selected sandbox.

Use:
  sandbox-cli manager help OPERATION
```

## Search And Suggestions

Exact operation lookup is primary:

```text
sandbox-cli runtime help exec_command
```

If the operation is unknown, do not silently render the overview. Return a usage
error and include compact search suggestions. Search should match:

- operation name;
- family title;
- operation summary;
- operation description;
- argument names;
- argument help;
- examples.

Unknown operation example:

```text
unknown runtime operation for help: exec

Did you mean:
  exec_command
    Start a command in a workspace.

Use:
  sandbox-cli runtime help
```

The first implementation can expose suggestions only on failed lookup. A later
explicit search command can be added if needed:

```text
sandbox-cli runtime help search transcript
```

Do not add the explicit `search` form in the first implementation unless the
operation lookup and suggestion behavior are already complete.

## Operation Metadata Assignments

Manager operation family assignments:

```text
Sandbox Lifecycle
  create_sandbox
  destroy_sandbox
  list_sandboxes
  inspect_sandbox

Sandbox Daemon Control
  start_sandbox_daemon
  stop_sandbox_daemon

Catalog Discovery
  describe_manager_operations
  describe_daemon_operations
```

Runtime operation family assignments:

```text
Command
  exec_command
  write_command_stdin
  poll_command
  read_command_lines
  cancel_command
```

Initial related operation assignments:

```text
create_sandbox
  list_sandboxes
  inspect_sandbox
  start_sandbox_daemon
  destroy_sandbox

destroy_sandbox
  list_sandboxes
  inspect_sandbox

list_sandboxes
  inspect_sandbox
  create_sandbox

inspect_sandbox
  list_sandboxes
  start_sandbox_daemon
  stop_sandbox_daemon

start_sandbox_daemon
  inspect_sandbox
  describe_daemon_operations
  stop_sandbox_daemon

stop_sandbox_daemon
  inspect_sandbox
  start_sandbox_daemon

describe_manager_operations
  describe_daemon_operations

describe_daemon_operations
  describe_manager_operations
  start_sandbox_daemon

exec_command
  poll_command
  write_command_stdin
  read_command_lines
  cancel_command

write_command_stdin
  exec_command
  poll_command
  read_command_lines
  cancel_command

poll_command
  exec_command
  read_command_lines
  cancel_command

read_command_lines
  exec_command
  poll_command

cancel_command
  exec_command
  poll_command
```

## Files To Change

Protocol:

```text
crates/sandbox-protocol/src/operation_spec.rs
crates/sandbox-protocol/src/catalog.rs
crates/sandbox-protocol/src/manual.rs -> help.rs
crates/sandbox-protocol/src/lib.rs
crates/sandbox-protocol/tests/unit.rs
```

Manager catalog:

```text
crates/sandbox-manager/src/operation/specs.rs
crates/sandbox-manager/src/operation/impls/mod.rs
crates/sandbox-manager/src/operation/impls/sandbox_lifecycle/*.rs
crates/sandbox-manager/src/operation/impls/daemon_control/*.rs
crates/sandbox-manager/src/operation/impls/catalog_discovery/*.rs
crates/sandbox-manager/tests/manager_core.rs
```

Runtime catalog:

```text
crates/sandbox-runtime/operation/src/public/command/service/impls/*.rs
crates/sandbox-runtime/operation/tests/service_graph.rs
```

Gateway CLI:

```text
crates/sandbox-gateway/src/cli/output.rs
crates/sandbox-gateway/src/cli/request_builder.rs
crates/sandbox-gateway/tests/gateway_cli.rs
```

Docs:

```text
docs/refactoring/sandbox-cli.md
docs/refactoring/sandbox-protocol.md
docs/README/sandbox-runtime.md
```

## Migration Steps

1. Add family/detail/related fields to protocol operation metadata and catalog
   document types.
2. Rename protocol manual renderer vocabulary to help.
3. Update catalog JSON conversion/parsing and validation.
4. Add manager operation family constants and attach each manager operation to
   one family.
5. Move manager operation implementations under family folders in
   `crates/sandbox-manager/src/operation/impls`.
6. Add one runtime `Command` family and attach every current runtime operation
   to it.
7. Replace `sandbox-cli manual` with scoped `manager help` and `runtime help`.
8. Add exact operation help rendering.
9. Add unknown-operation suggestions.
10. Update usage/examples so runtime help output does not display
   `--sandbox-id`.
11. Update active docs and focused tests.

## Acceptance Criteria

- `sandbox-cli manager help` renders grouped manager help.
- `sandbox-cli manager help create_sandbox` renders a detailed operation page.
- `sandbox-cli runtime help` renders grouped runtime help with one `Command`
  family.
- `sandbox-cli runtime help exec_command` renders a detailed operation page.
- Runtime help output does not show `--sandbox-id` in the command examples or
  usage lines.
- `sandbox-cli manual` is not accepted.
- `sandbox-cli manager help unknown` exits with usage failure and suggestions.
- `sandbox-cli runtime help unknown` exits with usage failure and suggestions.
- Catalog JSON includes `families` and every operation references a valid
  family.
- Catalog decoding rejects invalid family/related metadata.
- Manager operation implementation files are grouped under family directories
  below `crates/sandbox-manager/src/operation/impls`.
- Manager `impls/mod.rs` is only a family aggregator, not a flat operation list.
- Manager catalog still contains only manager operations.
- Runtime catalog still contains only runtime operations.

## Verification

```sh
cargo fmt --check -p sandbox-protocol -p sandbox-manager -p sandbox-gateway -p sandbox-runtime
cargo check -p sandbox-protocol -p sandbox-manager -p sandbox-gateway -p sandbox-runtime --tests
cargo test -p sandbox-protocol -p sandbox-manager -p sandbox-gateway -p sandbox-runtime
```
