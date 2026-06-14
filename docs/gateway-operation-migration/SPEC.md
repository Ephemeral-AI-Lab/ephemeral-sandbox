# Gateway Operation Migration Spec

This spec defines the operation split for the gateway migration. It replaces the
ambiguous current shape where host-served operations still use `sandbox.*`
names.

## Goal

Make operation names reveal where the operation executes:

| Prefix | Served by | Executes where | Owns |
|---|---|---|---|
| `host.*` | gateway/host | Host process | Docker images, containers, sandbox registry, gateway audit/query surfaces. |
| `sandbox.*` | daemon | Inside one sandbox container | Files, commands, plugins, isolation, LayerStack, workspace state, daemon runtime control. |

The gateway remains a transport/router. Host lifecycle logic belongs in the
host crate. Sandbox work stays behind the daemon socket.

## Naming Rules

1. Use `host.*` when the operation can be answered or completed without
   invoking a sandbox daemon.
2. Use `sandbox.*` when the operation needs one sandbox daemon.
3. Gateway calls to `sandbox.*` must include `sandbox_id`.
4. Gateway calls to `host.*` should not include `sandbox_id` unless the host
   operation is explicitly about an existing managed sandbox record.
5. Direct daemon calls never use host image/container/sandbox fleet operations.

## Contract Placement

After the crate layout refactor, the cross-boundary catalog and wire schemas
should live under shared contract crates:

```text
crates/shared/protocol   # op catalog, envelope, fault/error vocabulary
crates/shared/trace      # trace ids, records, batches, sidecar codec/constants
```

Local contracts stay with their owner:

```text
crates/gateway/contract  # gateway socket/operator-surface DTOs only
crates/host/contract     # host image/container/sandbox DTOs
crates/host/trace        # durable trace store, query, verify
crates/daemon/contract   # daemon-local runtime contracts
crates/daemon/trace      # daemon trace production, sidecar, spool, export/ack
```

The first-pass crate layout now exists. The live catalog source is
`crates/shared/protocol/src/catalog.rs`, and the generated catalog artifact is
`crates/daemon/operation/ops.json`.

## Top-Level Migration Plan

The migration should make the workspace layout match the runtime boundary:
shared contracts first, then gateway, host, and daemon implementation groups.

Target layout:

```text
crates/
  shared/
    protocol/          # original crates/protocol plus wire protocol prose/fixtures
    trace/             # original shared parts of crates/trace

  gateway/
    src/               # gateway crate root; no gateway/gateway duplicate
    contract/          # gateway-local request/socket DTOs, if split out
    trace/             # gateway-local access/transport traces, only if needed

  host/
    src/               # host crate root; no host/host duplicate
    contract/          # host image/container/sandbox DTOs, if split out
    trace/             # current host trace_store

  daemon/
    eosd/              # current crates/eosd
    core/              # current crates/daemon package, named daemon
    operation/         # current crates/operation
    layerstack/        # current crates/layerstack
    overlay/           # current crates/overlay
    workspace/         # current crates/workspace
    namespace/         # current crates/namespace
    command/           # current crates/command
    plugin/            # current crates/plugin, if restored as a separate crate
    config/            # current crates/config

  tests/
    e2e-test/          # current crates/e2e-test, optional final location
```

Package names may stay stable during the first move. For example, the package in
`crates/shared/protocol/Cargo.toml` can still be named `protocol`, and the
package in `crates/daemon/layerstack/Cargo.toml` can still be named
`layerstack`. Rename packages only after the physical grouping and dependency
rules are stable.

### Original-to-Target Map

| Original path | Target path | Owner | Notes |
|---|---|---|---|
| `crates/protocol` | `crates/shared/protocol` | shared | Cross-boundary op catalog, envelope, fault/error vocabulary. |
| `crates/trace` | `crates/shared/trace` | shared | Keep only trace ids, records, batches, codec, sidecar constants, and pure helpers here. |
| `crates/gateway` | `crates/gateway` | gateway | Transport, visibility enforcement, routing. No Docker/runtime fleet logic. |
| `crates/host` | `crates/host` | host | Docker runtime, image/container/sandbox registry, host trace persistence. |
| `crates/host/src/trace_store` | `crates/host/trace` or host module | host | Durable trace store/query/verify belongs to host, not shared trace. |
| `crates/daemon` | `crates/daemon/core` | daemon | Async daemon control plane and dispatch; package name can remain `daemon`. |
| `crates/eosd` | `crates/daemon/eosd` | daemon | Binary entrypoint deployed into containers. |
| `crates/operation` | `crates/daemon/operation` | daemon | Daemon operation DTOs and handlers. |
| `crates/layerstack` | `crates/daemon/layerstack` | daemon | Keep separate crate; do not merge into daemon control plane. |
| `crates/overlay` | `crates/daemon/overlay` | daemon | Keep separate crate for mount/syscall invariants. |
| `crates/workspace` | `crates/daemon/workspace` | daemon | Sandbox workspace orchestration. |
| `crates/namespace` | `crates/daemon/namespace` | daemon | Namespace holder/runner support and syscall boundaries. |
| `crates/command` | `crates/daemon/command` | daemon | PTY/process command lifecycle. |
| `crates/plugin` | `crates/daemon/plugin` | daemon | Plugin manifest/service/PPC contracts and daemon-side plugin support, if restored as a separate crate. |
| `crates/config` | `crates/daemon/config` | daemon | Daemon/runtime config consumed by daemon-side crates. |
| `crates/e2e-test` | `crates/tests/e2e-test` | tests | Optional; can stay top-level until runtime crate moves settle. |
| `contract/PROTOCOL.md` | `crates/shared/protocol/PROTOCOL.md` | shared | Active protocol contract lives with shared protocol, not as an unowned global root. |
| `contract/fixtures/wire_messages/*` | `crates/shared/protocol/fixtures/wire_messages/*` | shared | Wire fixtures are shared protocol conformance data. |
| `contract/fixtures/cas/cases.json` | `crates/daemon/layerstack/tests/fixtures/cas/cases.json` | daemon | CAS byte identity belongs to LayerStack. |
| `contract/fixtures/command_finalize_conflict_response.json` | `crates/daemon/operation/fixtures/command_finalize_conflict_response.json` | daemon | Command operation response fixture belongs to daemon operation. |
| `contract/fixtures/{audit_reset_floor_allowed,isolated_workspace_audit,layer_metrics}` | `crates/daemon/operation/fixtures/historical/*` | daemon | Historical daemon operation fixtures retained with operation. |

### Migration Phases

| Phase | Change | Acceptance gate |
|---|---|---|
| 0. Freeze baseline | Record current crate graph, op catalog, and generated docs before moves. | `cargo metadata`, current `ops.json`, and docs diff are captured. |
| 1. Introduce `shared` group | Move `crates/protocol` to `crates/shared/protocol`; move shared trace format code to `crates/shared/trace`. | Workspace builds with old package names and updated workspace paths. Completed in the first pass. |
| 2. Split trace ownership | Keep shared trace schema/codec in `shared/trace`; keep host trace store under host; keep daemon trace producer/spool/export under daemon. | No host trace-store code is imported by daemon; no daemon trace runtime is imported by host. |
| 3. Group gateway and host | Move gateway and host crates under `crates/gateway` and `crates/host`; add local contract modules only for owner-specific DTOs. | Gateway depends only on host plus shared crates. |
| 4. Group daemon crates | Move daemon-side crates under `crates/daemon`; keep `layerstack`, `overlay`, `workspace`, `namespace`, `command`, and `plugin` as separate crates. | Host/gateway do not depend on any `crates/daemon/*` package. |
| 5. Move active global contracts | Retire top-level active `contract/` by moving fixtures/prose to their owning shared, host, or daemon crate. | Contract tests read fixtures from owner-local paths. |
| 6. Rename host-served ops | Add `host.*` names for existing host-served operations. | Catalog contains the `host.*` names and docs prefer `host.*`. |
| 7. Add new gateway ops | Add image/profile/container host operations and host routing. | `host.*` ops do not route through a daemon and enforce visibility/policy. |
| 8. Remove compatibility aliases | Drop old host-served `sandbox.*` aliases. | Old names fail as unknown ops; docs and generated catalog contain only target names. Completed in cleanup. |

### Dependency Laws

| Rule | Enforced shape |
|---|---|
| Shared crates are leaves. | `shared/protocol` and `shared/trace` must not depend on gateway, host, or daemon implementation crates. |
| Gateway is a router. | Gateway may depend on shared crates and host, but not daemon implementation crates. |
| Host owns fleet state. | Host may depend on shared crates, but not daemon implementation crates. |
| Daemon owns sandbox state. | Daemon-side crates may depend on shared crates and other daemon-side crates, but not host/gateway. |
| Local contracts stay local. | Gateway/host/daemon `contract` modules are not generic dumping grounds; only owner-specific DTOs go there. |
| Shared trace is schema, not storage. | Shared trace owns ids/records/codec; host owns persistence/query/verify; daemon owns production/export. |

## Gateway Ops Migration Table

These operations are served by the host. Their old `sandbox.*` spellings are
retired; callers must use the `host.*` spellings.

| Current op | Target op | Served by | Surface | Mutates | Request target | Migration action |
|---|---|---:|---|---:|---|---|
| `sandbox.acquire` | `host.sandbox.acquire` | host | public | yes | Host registry/runtime | Old spelling removed. |
| `sandbox.release` | `host.sandbox.release` | host | public | yes | Existing managed sandbox record | Old spelling removed. Requires `sandbox_id` because it targets a host registry record. |
| `sandbox.status` | `host.sandbox.status` | host | public | no | Existing managed sandbox record | Old spelling removed. Requires `sandbox_id` because it targets a host registry record. |
| `sandbox.list` | `host.sandbox.list` | host | public | no | Host registry | Old spelling removed. No `sandbox_id`. |
| `sandbox.trace.requests` | `host.trace.requests` | host | operator | no | Host trace store | Old spelling removed. No daemon call. |
| `sandbox.trace.show` | `host.trace.show` | host | operator | no | Host trace store | Old spelling removed. No daemon call. |
| `sandbox.trace.verify` | `host.trace.verify` | host | operator | no | Host trace store | Old spelling removed. No daemon call. |

## New Gateway Ops Table

These are new host/gateway operations. They must be cataloged as `served_by:
host` and implemented in the host side, not in the sandbox daemon.

| New op | Served by | Surface | Mutates | Request target | Purpose | Required host behavior |
|---|---|---:|---|---:|---|---|
| `host.image_profiles.list` | host | public | no | Host policy | List approved image profiles that public clients may request. | Return only operator-approved aliases/profiles, not arbitrary local Docker images. |
| `host.image.list` | host | operator | no | Docker host | List locally available images visible to the gateway host. | Include image id/ref/tags/platform metadata where available. |
| `host.image.pull` | host | operator | yes | Docker host | Pull or refresh an operator-approved image reference. | Enforce image policy before pulling; record pull outcome in host trace/audit. |
| `host.container.list` | host | operator | no | Docker host | List containers relevant to the gateway host. | Include both managed sandbox containers and compatible unmanaged candidates. |
| `host.container.start` | host | operator | yes | Docker host | Start a container from an explicit image reference and optional name. | Bootstrap `eosd`, create/register runtime endpoint metadata, return managed identity if adopted. |
| `host.container.adopt` | host | operator | yes | Existing container | Register an existing compatible container as a managed sandbox. | Verify daemon compatibility, socket/endpoint readiness, platform, and image policy before registry insert. |
| `host.container.stop` | host | operator | yes | Existing container or sandbox | Stop a container by container name/id or managed `sandbox_id`. | Resolve host target, stop container, update registry state if managed. |
| `host.container.remove` | host | operator | yes | Existing container or sandbox | Remove a container by container name/id or managed `sandbox_id`. | Stop first when requested/needed; delete registry record if managed. |

## Sandbox Ops That Stay `sandbox.*`

Daemon-served operations keep the `sandbox.*` prefix. The gateway may route them,
but it must route by `sandbox_id` to exactly one sandbox daemon.

| Family | Examples | Served by | Why it stays sandbox-owned |
|---|---|---:|---|
| Control | `sandbox.call.heartbeat`, `sandbox.call.cancel`, `sandbox.call.count` | daemon | Reads or mutates daemon invocation state. |
| Files | `sandbox.file.read`, `sandbox.file.write`, `sandbox.file.edit` | daemon | Reads or mutates sandbox workspace/LayerStack state. |
| Plugins | `sandbox.plugin.*` | daemon | Talks to plugin services running in or for one sandbox. |
| Isolation | `sandbox.isolation.*` | daemon | Manages isolated workspace state inside one sandbox. |
| Command | `sandbox.command.*` | daemon | Manages command processes inside one sandbox. |
| Run cleanup | `sandbox.run.end`, `sandbox.run.cancel_all` | daemon | Owns caller/workspace-run state inside one sandbox. |
| Checkpoint | `sandbox.checkpoint.*` | daemon | Reads or mutates LayerStack/workspace binding state. |
| Trace export | `sandbox.trace.export`, `sandbox.trace.export_ack` | daemon | Internal daemon-to-host trace drain protocol for one sandbox. |
| Runtime readiness | `sandbox.runtime.ready` | daemon | Internal daemon readiness probe for one sandbox. |

## Request Shapes

Host operation with no sandbox target:

```json
{
  "op": "host.image_profiles.list",
  "invocation_id": "req-1",
  "args": {}
}
```

Host operation targeting a managed sandbox record:

```json
{
  "op": "host.sandbox.status",
  "sandbox_id": "sb-123",
  "invocation_id": "req-2",
  "args": {}
}
```

Sandbox operation routed through the gateway:

```json
{
  "op": "sandbox.command.exec",
  "sandbox_id": "sb-123",
  "invocation_id": "req-3",
  "args": {
    "cmd": "pwd",
    "layer_stack_root": "/eos/layer-stack"
  }
}
```

## Acquire Semantics

`host.sandbox.acquire` is the public safe path for client-selected runtime
choice. Public clients should request an approved profile, not arbitrary Docker
image names.

```json
{
  "op": "host.sandbox.acquire",
  "invocation_id": "req-4",
  "args": {
    "image_profile": "python-3.12",
    "name_hint": "debug-run"
  }
}
```

The host owns:

| Host responsibility | Reason |
|---|---|
| Profile-to-image resolution | Prevents public clients from selecting arbitrary images. |
| Docker pull/start policy | Docker is host state, not daemon state. |
| Container naming | Avoids client-controlled global Docker names. |
| `eosd` bootstrap | The daemon is deployed into a container by host machinery. |
| Registry insert/update | Gateway routes by host-managed `sandbox_id`. |
| Trace/audit record | Host is the durable side across container restarts/removals. |

## Compatibility Plan

Compatibility aliases are removed. Old host-served `sandbox.*` spellings now
fail as `unknown_op`; docs and the generated catalog contain only target
`host.*` names for host-served operations.

## Implementation Checklist

1. Add `host.*` entries to the shared protocol catalog.
2. Extend host verb routing for the new host ops.
3. Implement host image profile policy and Docker image/container adapters.
4. Keep public image selection profile-based; reserve raw image references for
   operator operations.
5. Regenerate `crates/daemon/operation/ops.json` and docs from the catalog.
6. Add gateway tests proving `host.*` ops do not require daemon routing.
7. Add gateway tests proving `sandbox.*` ops require `sandbox_id`.
8. Run the contract drift gate after catalog/doc changes.

## Non-Goals

| Non-goal | Reason |
|---|---|
| Let public clients pass arbitrary Docker images to acquire. | This bypasses host policy and makes image trust a client decision. |
| Implement image/container operations in `daemon`. | The daemon runs inside one container and cannot own host Docker state. |
| Move trace persistence into daemon. | Containers are ephemeral; host trace store is the durable operator view. |
| Treat `shared` as a utility bucket. | Shared crates are only for cross-boundary contracts that must not drift. |
