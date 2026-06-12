# Sandbox API — op catalog

GENERATED from `crates/eos-operation/ops.json` by `cargo run -p xtask -- gen-docs`.
Do not edit by hand: `cargo run -p xtask -- check-contract` fails when
this file drifts from the committed catalog.

Protocol version: **1**

## Public ops (client socket)

The complete public vocabulary served on the `eos-sandbox-gateway` client socket.

| Op | Served by | Family | Mutates | Summary |
|---|---|---|---|---|
| `sandbox.acquire` | host | Sandbox | yes | Provision a sandbox container plus daemon and return its sandbox_id. |
| `sandbox.release` | host | Sandbox | yes | Destroy the sandbox container and drop its registry entry. |
| `sandbox.status` | host | Sandbox | no | Host view of one sandbox (container/endpoint/recovery state) plus embedded daemon readiness. |
| `sandbox.list` | host | Sandbox | no | Enumerate the sandbox registry. |
| `sandbox.call.heartbeat` | daemon | Control | yes | Extend the lease on an in-flight invocation. |
| `sandbox.call.cancel` | daemon | Control | yes | Request cooperative cancellation of an in-flight invocation. |
| `sandbox.call.count` | daemon | Control | no | Count in-flight invocations. |
| `sandbox.file.read` | daemon | Files | no | Read one file from the layer stack or isolated workspace. |
| `sandbox.file.write` | daemon | Files | yes | Write one file through the OCC gate. |
| `sandbox.file.edit` | daemon | Files | yes | Edit one file through the OCC gate. |
| `sandbox.plugin.ensure` | daemon | Plugins | yes | Ensure a plugin service is installed and running. |
| `sandbox.plugin.status` | daemon | Plugins | no | Inspect plugin service status. |
| `sandbox.isolation.enter` | daemon | IsolatedWorkspace | yes | Enter isolated workspace mode for a caller. |
| `sandbox.isolation.exit` | daemon | IsolatedWorkspace | yes | Exit isolated workspace mode for a caller. |
| `sandbox.isolation.status` | daemon | IsolatedWorkspace | no | Inspect isolated workspace status. |
| `sandbox.command.exec` | daemon | Command | yes | Run a foreground command or start a background command. |
| `sandbox.command.write_stdin` | daemon | Command | yes | Write stdin to a command. |
| `sandbox.command.poll` | daemon | Command | no | Poll command progress without writing stdin. |
| `sandbox.command.cancel` | daemon | Command | yes | Cancel a command. |
| `sandbox.command.collect_completed` | daemon | Command | yes | Collect completed command notifications. |
| `sandbox.command.count` | daemon | Command | no | Count live commands. |
| `sandbox.run.end` | daemon | WorkspaceRun | yes | End a run: cancel every workspace run owned by one caller (caller_id == agent_run_id), discarding its commands and exiting its isolated workspace. |

## Operator ops (operator socket)

Served only on the operator socket beside the client socket; never the client socket.

| Op | Served by | Family | Mutates | Summary |
|---|---|---|---|---|
| `sandbox.checkpoint.layer_metrics` | daemon | Checkpoint | no | Report LayerStack and storage metrics for the sandbox. |
| `sandbox.checkpoint.ensure_base` | daemon | Checkpoint | yes | Ensure a workspace base binding exists. |
| `sandbox.checkpoint.build_base` | daemon | Checkpoint | yes | Build or rebuild a workspace base binding. |
| `sandbox.checkpoint.commit_to_workspace` | daemon | Checkpoint | yes | Materialize LayerStack state into the bound workspace. |
| `sandbox.checkpoint.commit_to_git` | daemon | Checkpoint | yes | Commit a LayerStack snapshot into the bound workspace's durable Git repo. |
| `sandbox.checkpoint.binding` | daemon | Checkpoint | no | Inspect the workspace binding for a layer stack root. |
| `sandbox.isolation.list_open` | daemon | IsolatedWorkspace | no | List open isolated workspaces. |
| `sandbox.run.cancel_all` | daemon | WorkspaceRun | yes | Cancel every workspace run in the sandbox: the whole-sandbox sweep backstop. |

## Internal ops

Reserved for the host recovery machine; not served from any socket.

| Op | Served by | Family | Mutates | Summary |
|---|---|---|---|---|
| `sandbox.runtime.ready` | daemon | Control | no | Daemon readiness probe used by the host recovery machine. |
| `sandbox.trace.export` | daemon | Control | no | Drain bounded daemon background trace records for host ingest. |

## Test ops

Daemon-side test hooks; refused by `eos-sandbox-gateway` and exercised only by direct-daemon test harnesses.

| Op | Served by | Family | Mutates | Summary |
|---|---|---|---|---|
| `sandbox.isolation.test_reset` | daemon | IsolatedWorkspace | yes | Test-only isolated workspace reset hook. |

## Dynamic plugin ops

`plugin.<id>.<op>` names are registered at runtime by plugin services inside a sandbox. They are daemon-served, public, and treated as mutating (fail-closed) by the recovery ladder; they never appear in the static catalog.
