# Sandbox API — op catalog

GENERATED from `crates/daemon/operation/ops.json` by `cargo run -p xtask -- gen-docs`.
Do not edit by hand: `cargo run -p xtask -- check-contract` fails when
this file drifts from the committed catalog.

Protocol version: **1**

## Public ops (client socket)

The complete public vocabulary served on the `gateway` client socket.

| Op | Served by | Family | Mutates | Args DTO | Response DTO | Summary |
|---|---|---|---|---|---|---|
| `host.sandbox.acquire` | host | Sandbox | yes | `host.sandbox.AcquireArgs` | `host.sandbox.AcquireResponse` | Provision a sandbox container plus daemon and return its sandbox_id. |
| `host.sandbox.release` | host | Sandbox | yes | `host.sandbox.ReleaseArgs` | `host.sandbox.ReleaseResponse` | Destroy the sandbox container and drop its registry entry. |
| `host.sandbox.status` | host | Sandbox | no | `host.sandbox.StatusArgs` | `host.sandbox.StatusResponse` | Host view of one sandbox (container/endpoint/recovery state) plus embedded daemon readiness. |
| `host.sandbox.list` | host | Sandbox | no | `host.sandbox.ListArgs` | `host.sandbox.ListResponse` | Enumerate the sandbox registry. |
| `host.image_profiles.list` | host | Image | no | `host.image_profiles.ListArgs` | `host.image_profiles.ListResponse` | List operator-approved image profiles that public clients may request. |
| `sandbox.call.heartbeat` | daemon | Control | yes | `operation.control.HeartbeatInput` | `operation.control.HeartbeatOutput` | Extend the lease on an in-flight invocation. |
| `sandbox.call.cancel` | daemon | Control | yes | `operation.control.CancelInvocationInput` | `operation.control.CancelInvocationOutput` | Request cooperative cancellation of an in-flight invocation. |
| `sandbox.call.count` | daemon | Control | no | `operation.control.CallerCountInput` | `operation.control.InflightCountOutput` | Count in-flight invocations. |
| `sandbox.file.read` | daemon | Files | no | `operation.file.ReadFileInput` | `operation.file.ReadFileResponse` | Read one file from the layer stack or isolated workspace. |
| `sandbox.file.write` | daemon | Files | yes | `operation.file.WriteFileInput` | `operation.file.WriteFileResponse` | Write one file through the OCC gate. |
| `sandbox.file.edit` | daemon | Files | yes | `operation.file.EditFileInput` | `operation.file.EditFileResponse` | Edit one file through the OCC gate. |
| `sandbox.plugin.list` | daemon | Plugins | no | `operation.plugin.PluginListInput` | `operation.plugin.PluginListOutput` | List configured first-party plugin providers without probing them. |
| `sandbox.plugin.health` | daemon | Plugins | no | `operation.plugin.PluginHealthInput` | `operation.plugin.PluginHealthOutput` | Actively probe enabled first-party plugin providers. |
| `sandbox.plugin.pyright_lsp.query_symbols` | daemon | Plugins | no | `operation.plugin.PyrightLspQuerySymbolsInput` | `operation.plugin.PyrightLspQuerySymbolsOutput` | Return Pyright document symbols for a Python file. |
| `sandbox.plugin.pyright_lsp.definition` | daemon | Plugins | no | `operation.plugin.PyrightLspDefinitionInput` | `operation.plugin.PyrightLspLocationsOutput` | Resolve a Pyright definition location. |
| `sandbox.plugin.pyright_lsp.references` | daemon | Plugins | no | `operation.plugin.PyrightLspReferencesInput` | `operation.plugin.PyrightLspLocationsOutput` | Resolve Pyright reference locations. |
| `sandbox.plugin.pyright_lsp.diagnostics` | daemon | Plugins | no | `operation.plugin.PyrightLspDiagnosticsInput` | `operation.plugin.PyrightLspDiagnosticsOutput` | Return current Pyright diagnostics for a Python file. |
| `sandbox.isolation.enter` | daemon | IsolatedWorkspace | yes | `operation.isolation.IsolationEnterInput` | `operation.isolation.IsolationEnterOutput` | Enter isolated workspace mode for a caller. |
| `sandbox.isolation.exit` | daemon | IsolatedWorkspace | yes | `operation.isolation.IsolationExitInput` | `operation.isolation.IsolationExitOutput` | Exit isolated workspace mode for a caller. |
| `sandbox.isolation.status` | daemon | IsolatedWorkspace | no | `operation.isolation.IsolationStatusInput` | `operation.isolation.IsolationStatusOutput` | Inspect isolated workspace status. |
| `sandbox.command.exec` | daemon | Command | yes | `operation.command.ExecCommandInput` | `operation.command.CommandResponse` | Run a foreground command or start a background command. |
| `sandbox.command.write_stdin` | daemon | Command | yes | `operation.command.WriteStdinInput` | `operation.command.CommandResponse` | Write stdin to a command. |
| `sandbox.command.poll` | daemon | Command | yes | `operation.command.ReadProgressInput` | `operation.command.CommandResponse` | Poll command progress without writing stdin and finalize completed commands. |
| `sandbox.command.cancel` | daemon | Command | yes | `operation.command.CancelCommandInput` | `operation.command.CommandResponse` | Cancel a command. |
| `sandbox.command.collect_completed` | daemon | Command | yes | `operation.command.CollectCompletedInput` | `operation.command.CollectCompletedOutput` | Collect completed command notifications. |
| `sandbox.command.count` | daemon | Command | no | `operation.control.CallerCountInput` | `operation.command.CommandCountOutput` | Count live commands. |
| `sandbox.run.end` | daemon | WorkspaceRun | yes | `operation.workspace_run.RunEndInput` | `operation.workspace_run.RunEndOutput` | End a run: cancel every workspace run owned by one caller (caller_id == agent_run_id), discarding its commands and exiting its isolated workspace. |

## Operator ops (operator socket)

Served only on the operator socket beside the client socket; never the client socket.

| Op | Served by | Family | Mutates | Args DTO | Response DTO | Summary |
|---|---|---|---|---|---|---|
| `host.trace.requests` | host | Trace | no | `host.trace.TraceRequestsArgs` | `host.trace.TraceRequestsResponse` | List recent trace requests from the host audit store. |
| `host.trace.show` | host | Trace | no | `host.trace.TraceShowArgs` | `host.trace.TraceShowResponse` | Show one trace from the host audit projections. |
| `host.trace.verify` | host | Trace | no | `host.trace.TraceVerifyArgs` | `host.trace.TraceVerifyReport` | Verify host audit hash chains and projection joinability. |
| `host.image.list` | host | Image | no | `host.image.ListArgs` | `host.image.ListResponse` | List locally available Docker images visible to the gateway host. |
| `host.image.pull` | host | Image | yes | `host.image.PullArgs` | `host.image.PullResponse` | Pull or refresh an operator-approved image reference. |
| `host.container.list` | host | Container | no | `host.container.ListArgs` | `host.container.ListResponse` | List Docker containers relevant to the gateway host. |
| `host.container.start` | host | Container | yes | `host.container.StartArgs` | `host.container.StartResponse` | Start a container from an explicit image reference and optional name. |
| `host.container.adopt` | host | Container | yes | `host.container.AdoptArgs` | `host.container.AdoptResponse` | Register an existing compatible container as a managed sandbox. |
| `host.container.stop` | host | Container | yes | `host.container.StopArgs` | `host.container.StopResponse` | Stop a host container by container name/id or managed sandbox_id. |
| `host.container.remove` | host | Container | yes | `host.container.RemoveArgs` | `host.container.RemoveResponse` | Remove a host container by container name/id or managed sandbox_id. |
| `sandbox.checkpoint.layer_metrics` | daemon | Checkpoint | no | `operation.checkpoint.LayerMetricsInput` | `operation.checkpoint.LayerMetricsOutput` | Report LayerStack and storage metrics for the sandbox. |
| `sandbox.checkpoint.ensure_base` | daemon | Checkpoint | yes | `operation.checkpoint.EnsureBaseInput` | `operation.checkpoint.WorkspaceBaseOutput` | Ensure a workspace base binding exists. |
| `sandbox.checkpoint.build_base` | daemon | Checkpoint | yes | `operation.checkpoint.BuildBaseInput` | `operation.checkpoint.WorkspaceBaseOutput` | Build or rebuild a workspace base binding. |
| `sandbox.checkpoint.commit_to_workspace` | daemon | Checkpoint | yes | `operation.checkpoint.CommitToWorkspaceInput` | `operation.checkpoint.CommitToWorkspaceOutput` | Materialize LayerStack state into the bound workspace. |
| `sandbox.checkpoint.commit_to_git` | daemon | Checkpoint | yes | `operation.checkpoint.CommitInput` | `operation.checkpoint.CommitOutput` | Commit a LayerStack snapshot into the bound workspace's durable Git repo. |
| `sandbox.checkpoint.binding` | daemon | Checkpoint | no | `operation.checkpoint.BindingInput` | `operation.checkpoint.BindingOutput` | Inspect the workspace binding for a layer stack root. |
| `sandbox.isolation.list_open` | daemon | IsolatedWorkspace | no | `operation.core.NoArgs` | `operation.isolation.ListOpenOutput` | List open isolated workspaces. |
| `sandbox.run.cancel_all` | daemon | WorkspaceRun | yes | `operation.workspace_run.RunCancelAllInput` | `operation.workspace_run.RunCancelAllOutput` | Cancel every workspace run in the sandbox: the whole-sandbox sweep backstop. |

## Internal ops

Reserved for the host recovery machine; not served from any socket.

| Op | Served by | Family | Mutates | Args DTO | Response DTO | Summary |
|---|---|---|---|---|---|---|
| `sandbox.runtime.ready` | daemon | Control | no | `operation.control.RuntimeReadyInput` | `operation.control.RuntimeReadyOutput` | Daemon readiness probe used by the host recovery machine. |
| `sandbox.trace.export` | daemon | Control | no | `operation.control.TraceExportInput` | `operation.control.TraceExportOutput` | Lease bounded daemon background trace records for host ingest. |
| `sandbox.trace.export_ack` | daemon | Control | yes | `operation.control.TraceExportAckInput` | `operation.control.TraceExportAckOutput` | Ack a durably ingested daemon trace export lease. |

## Test ops

Daemon-side test hooks; refused by `gateway` and exercised only by direct-daemon test harnesses.

| Op | Served by | Family | Mutates | Args DTO | Response DTO | Summary |
|---|---|---|---|---|---|---|
| `sandbox.isolation.test_reset` | daemon | IsolatedWorkspace | yes | `operation.core.NoArgs` | `operation.isolation.TestResetOutput` | Test-only isolated workspace reset hook. |

## Plugin providers

First-party plugin providers are static catalog entries under `sandbox.plugin.*`. The initial provider is `sandbox.plugin.pyright_lsp.*`; dynamic plugin-op forwarding is not part of the public API.
