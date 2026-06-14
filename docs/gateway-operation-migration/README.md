# Gateway Operation Migration

This note separates host-side gateway operations from in-sandbox daemon
operations.

## Naming Rule

| Prefix | Owner | Meaning | `sandbox_id` |
|---|---|---|---|
| `host.*` | gateway/host | Docker, image, container, sandbox registry, gateway audit, fleet lifecycle | Usually absent; present only when referring to a managed sandbox record |
| `sandbox.*` | daemon | Work inside one sandbox container: files, commands, plugins, isolation, LayerStack, daemon control | Required for gateway calls, except direct daemon/internal test paths |

Rule of thumb:

| Question | Prefix |
|---|---|
| Does it talk to Docker, host state, images, container lifecycle, or gateway audit? | `host.*` |
| Does it execute inside a sandbox or mutate/read sandbox workspace state? | `sandbox.*` |

## Host Actions

These are host-served operations. The catalog uses `host.*`; old host-served
`sandbox.*` spellings are retired and fail as `unknown_op`.

| Current op | Surface | Mutates | Purpose |
|---|---|---|---|
| `host.sandbox.acquire` | public | yes | Provision a sandbox container plus daemon and return its `sandbox_id`. |
| `host.sandbox.release` | public | yes | Destroy a managed sandbox container and drop its registry record. |
| `host.sandbox.status` | public | no | Inspect one managed sandbox from host state, including daemon readiness. |
| `host.sandbox.list` | public | no | Enumerate host-managed sandboxes. |
| `host.trace.requests` | operator | no | List recent host audit requests. |
| `host.trace.show` | operator | no | Show one host audit trace projection. |
| `host.trace.verify` | operator | no | Verify host audit hash chains and projection joinability. |

## Current Sandbox Actions

These are daemon-served today and should keep the `sandbox.*` prefix.

| Op | Surface | Mutates | Purpose |
|---|---|---|---|
| `sandbox.call.heartbeat` | public | yes | Extend an in-flight invocation lease. |
| `sandbox.call.cancel` | public | yes | Request cooperative cancellation for an in-flight invocation. |
| `sandbox.call.count` | public | no | Count in-flight invocations. |
| `sandbox.file.read` | public | no | Read one file from the LayerStack or isolated workspace. |
| `sandbox.file.write` | public | yes | Write one file through the OCC gate. |
| `sandbox.file.edit` | public | yes | Edit one file through the OCC gate. |
| `sandbox.plugin.list` | public | no | List configured first-party plugin providers. |
| `sandbox.plugin.health` | public | no | Probe configured first-party plugin providers. |
| `sandbox.plugin.pyright_lsp.query_symbols` | public | no | Return Pyright document symbols for a Python file. |
| `sandbox.plugin.pyright_lsp.definition` | public | no | Resolve a Pyright definition location. |
| `sandbox.plugin.pyright_lsp.references` | public | no | Resolve Pyright reference locations. |
| `sandbox.plugin.pyright_lsp.diagnostics` | public | no | Return current Pyright diagnostics for a Python file. |
| `sandbox.isolation.enter` | public | yes | Enter isolated workspace mode for a caller. |
| `sandbox.isolation.exit` | public | yes | Exit isolated workspace mode for a caller. |
| `sandbox.isolation.status` | public | no | Inspect isolated workspace status. |
| `sandbox.command.exec` | public | yes | Run a foreground command or start a background command. |
| `sandbox.command.write_stdin` | public | yes | Write stdin to a running command. |
| `sandbox.command.poll` | public | yes | Poll command progress and finalize completed commands. |
| `sandbox.command.cancel` | public | yes | Cancel a command. |
| `sandbox.command.collect_completed` | public | yes | Collect completed command notifications. |
| `sandbox.command.count` | public | no | Count live commands. |
| `sandbox.run.end` | public | yes | End one caller-owned workspace run. |
| `sandbox.checkpoint.layer_metrics` | operator | no | Report LayerStack and storage metrics. |
| `sandbox.checkpoint.ensure_base` | operator | yes | Ensure a workspace base binding exists. |
| `sandbox.checkpoint.build_base` | operator | yes | Build or rebuild a workspace base binding. |
| `sandbox.checkpoint.commit_to_workspace` | operator | yes | Materialize LayerStack state into the bound workspace. |
| `sandbox.checkpoint.commit_to_git` | operator | yes | Commit LayerStack state into the durable workspace Git repo. |
| `sandbox.checkpoint.binding` | operator | no | Inspect a LayerStack workspace binding. |
| `sandbox.isolation.list_open` | operator | no | List open isolated workspaces. |
| `sandbox.run.cancel_all` | operator | yes | Cancel every workspace run in one sandbox. |
| `sandbox.runtime.ready` | internal | no | Daemon readiness probe used by host recovery. |
| `sandbox.trace.export` | internal | no | Lease daemon trace records for host ingest. |
| `sandbox.trace.export_ack` | internal | yes | Ack durably ingested daemon trace records. |
| `sandbox.isolation.test_reset` | test | yes | Test-only isolated workspace reset hook. |

## New Host Actions

These are new host/gateway operations. They should not execute inside a sandbox
daemon.

| Proposed op | Surface | Mutates | Purpose |
|---|---|---|---|
| `host.image_profiles.list` | public | no | List operator-approved image profiles that public clients may request. |
| `host.image.list` | operator | no | List locally available Docker images visible to the gateway host. |
| `host.image.pull` | operator | yes | Pull or refresh an operator-approved image reference. |
| `host.container.list` | operator | no | List Docker containers relevant to the gateway host. |
| `host.container.start` | operator | yes | Start a container from an explicit image reference and optional name. |
| `host.container.adopt` | operator | yes | Register an existing compatible container as a managed sandbox. |
| `host.container.stop` | operator | yes | Stop a host container by container name or managed `sandbox_id`. |
| `host.container.remove` | operator | yes | Remove a host container by container name or managed `sandbox_id`. |

## Enriched Acquire

`host.sandbox.acquire` is the public safe path for client-selected runtime
choice:

```json
{
  "op": "host.sandbox.acquire",
  "invocation_id": "req-1",
  "args": {
    "image_profile": "python-3.12",
    "name_hint": "debug-run"
  }
}
```
The host still owns the real Docker image mapping, container naming, daemon
bootstrap, registry insert, recovery metadata, and returned `sandbox_id`.

## Request Shape

Host/fleet request:

```json
{
  "op": "host.image_profiles.list",
  "invocation_id": "req-1",
  "args": {}
}
```

Sandbox/daemon request through gateway:

```json
{
  "op": "sandbox.command.exec",
  "sandbox_id": "sb-...",
  "invocation_id": "req-2",
  "args": {
    "cmd": "pwd",
    "layer_stack_root": "/eos/layer-stack"
  }
}
```
