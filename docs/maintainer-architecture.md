# Maintainer architecture

This document defines component ownership and dependency boundaries for
Ephemeral Sandbox. It is intended for maintainers; the top-level README stays
focused on using the project.

## Request path

```text
operator or agent
   | sandbox-manager-cli / sandbox-runtime-cli / sandbox-observability-cli
   | sandbox-mcp --set management|runtime|observability
   v
sandbox-operation-catalog + adapter-owned projection
   | adapter builds an operation-contract request
   v
sandbox-operation-client
   | authenticated newline-delimited JSON via sandbox-protocol
   v
sandbox-gateway
   v
sandbox-manager
   | handles system routes and forwards sandbox routes
   v
sandbox-daemon
   | decodes wire requests and composes applications
   v
sandbox-runtime / sandbox-observability-query
   | command, file, workspace, layerstack, and observability behavior
   v
sandbox-runtime-workspace / sandbox-runtime-layerstack /
sandbox-runtime-namespace-execution / sandbox-runtime-namespace-process /
sandbox-runtime-overlay / sandbox-observability-telemetry
```

## Component map

| Component | Kind | Job | Must never |
|---|---|---|---|
| `sandbox-operation-contract` | lib | Own adapter-neutral operation, argument, scope, route, request, response, and application-error types | Depend on any workspace package or own wire/presentation behavior |
| `sandbox-operation-catalog` | lib | Own canonical internal identifiers and routes unconditionally, plus every public declaration and route in feature-gated manager/runtime/observability modules | Depend on anything except the contract, own CLI metadata, or contain handlers |
| `sandbox-operation-client` | lib | Own gateway discovery and wire transport shared by product adapters, plus value-based request construction shared by CLI and MCP | Depend on the catalog, applications, adapters, or `sandbox-config` |
| `sandbox-gateway` | bin+lib | Compose the public gateway listener, manager application, Docker provider, daemon wire client, and local daemon installer | Own application behavior, depend on product adapters or the shared client, or compose runtime applications directly |
| `sandbox-cli` | lib + 3 bins | Own CLI paths, flags, positionals, help, output, and separately feature-gated manager/runtime/observability executables | Depend on protocol/applications/other adapters, provide a combined executable, or let one binary enumerate another authority |
| `sandbox-mcp` | bin | Project exactly one selected domain from the merged catalog as a stdio MCP server and send through the shared client | Define a second catalog, expose a combined set, or depend on protocol/applications/CLI |
| `sandbox-manager` | lib | Own sandbox lifecycle, daemon endpoint tracking, system-scoped operation handlers, routing, and application ports | Depend on protocol/client/adapters/composition roots or implement runtime command/workspace semantics |
| `sandbox-protocol` | lib | Own wire codec, framing, authentication fields, limits, and the daemon readiness handshake | Own operation declarations/help or depend on catalog/applications/client/adapters |
| `sandbox-daemon` | bin+lib | Compose authenticated RPC, the exact HTTP allowlist, runtime dispatch, observability dispatch, sampling, and lifecycle | Depend on product adapters/client/manager or expose operation routes over HTTP beyond `file_list` |
| `sandbox-observability-query` | lib | Own structured observability query selection and response construction through an application-owned input port | Depend on protocol/client/adapters/daemon or the concrete runtime application |
| `sandbox-observability-telemetry` | lib | Own tracing, events, sampling, collection, and reading primitives | Depend on any workspace package |
| `sandbox-runtime` | lib | Own public runtime handlers plus canonical internal workspace-session/layerstack dispatch and orchestration | Depend on protocol/client/adapters/composition roots or own low-level runtime primitives |
| `sandbox-runtime-workspace` | lib | Own workspace runtime lifecycle, namespace handles, capture, and destroy | Own command process state |
| `sandbox-runtime-layerstack` | lib | Own content hashes, manifest/layer types, storage, and leases | Own command execution |
| `sandbox-runtime-layerstack-core` | lib | Own safe, standard-library-only portable identity values, raw relative Linux path validation, canonical v2 records, bounded decoding, and narrow source/sink/digest ports | Depend on LayerStack, hashing/serde/filesystem/provider/runtime/benchmark crates, use unsafe/FFI, or own persistence, publication, capture, materialization, telemetry, or process state |
| `sandbox-runtime-namespace-execution` | lib | Own the namespace execution engine, PTY I/O, and transcript read/write windowing | Own workspace lifecycle |
| `sandbox-runtime-namespace-process` | lib | Own namespace holder/runner bodies and setns execution | Own operation dispatch |
| `sandbox-runtime-overlay` | lib | Own low-level overlay mount and unmount primitives | Own workspace lifecycle |
| `sandbox-config` | lib | Own sandbox YAML loading, merging, validation, and typed gateway/manager/daemon/observability/runner/runtime schemas | Depend on any workspace package or own runtime behavior |
| `sandbox-provider-docker` | lib | Implement manager ports with Docker and use protocol only for daemon readiness | Own generic lifecycle/rollback, application handlers, client behavior, or depend on `sandbox-daemon` |

## Boundary law

Semantic and application-envelope vocabulary lives in
`crates/sandbox-operations/contract`. Every public declaration, route, and
canonical internal identifier lives in `crates/sandbox-operations/catalog`.
Shared gateway client behavior lives in `crates/sandbox-operations/client`.
CLI metadata lives only in `crates/sandbox-cli/src/projection`. Wire-only codec,
framing, authentication, limits, and readiness live in
`crates/sandbox-protocol`.

Applications (`sandbox-manager`, `sandbox-runtime`, and
`sandbox-observability-query`) never depend on protocol, the client, product
adapters, composition roots, or each other's implementations. The contract,
config, telemetry, layerstack core, and overlay packages have no workspace
dependencies. LayerStack has the sole inward runtime edge to LayerStack core.
The catalog depends only on the contract, protocol depends only on the
contract, and the client depends only on contract and protocol. Portable-root
fixtures and concrete SHA-256/serde adapters live with
`sandbox-runtime-layerstack`.

## Portable root contract boundary

Stage 02 introduces one inward dependency and no runtime authority change:

```text
workspace/provider adapters
            |
            v
sandbox-runtime-layerstack
  capture preparation, whiteout filtering, bounded external ordering,
  SHA-256, serde diagnostics, storage, leases, v1 publication
            |
            v
sandbox-runtime-layerstack-core
  portable values, validation, canonical bytes, bounded source/sink ports
```

The reverse edge is forbidden. LayerStack core cannot name or import
filesystem paths, persistence APIs, fsync, providers, Docker, OverlayFS,
mounts, namespaces, runtime operations, telemetry, E2E or benchmark support,
async runtimes, services, helpers, SHA/serde implementations, FFI, or unsafe
code. Capture may present only already-canonical logical entries to the core;
LayerStack owns disk-run spooling, merge ordering, hardlink/reference claim
preparation, and exclusion of Linux whiteout/opaque carrier markers.

The core's `RootId`, `TreeManifestId`, and typed object IDs are portable
contract values only. Stage 02 does not persist, activate, publish, resolve,
or materialize them. `Manifest::root_hash` and the legacy v1 manifest remain
the sole runtime revision and read/write/publication authority. Provider
locators, native materialization generations, host paths, inode/carrier
identities, and Stage 01 workspace transcripts remain outside portable
identity. Future v2 persistence and materialization must stay outward of the
core and may become runtime-reachable only in their separately gated stages.

Exactly three organizational namespace directories exist under `crates/`:
`sandbox-operations/`, `sandbox-observability/`, and `sandbox-runtime/`. They
are grouping directories only and never gain a root `Cargo.toml`, Rust facade,
package identity, or re-export layer.

## Repository layout

- `crates/sandbox-operations/` groups `contract/`, `catalog/`, and `client/`.
- `crates/sandbox-observability/` groups `telemetry/` and `query/`.
- `crates/sandbox-runtime/` groups `operation/`, `workspace/`, `layerstack/`,
  `namespace-execution/`, `namespace-process/`, and `overlay/`.
- `crates/` also contains the flat CLI, config, daemon, gateway, manager, MCP,
  protocol, and Docker-provider packages.
- `crates/sandbox-runtime/layerstack/tests/fixtures/` owns runtime CAS fixtures.
- `e2e/` contains live CLI, MCP, gateway, manager, daemon, runtime, and
  observability coverage.
- `config/prd.yml` is the daemon configuration baseline.
- `dist/` contains packaged static binaries and supporting artifacts uploaded
  into sandbox containers.

## Public interface boundaries

The CLI has three executables: management, runtime, and observability. There is
no combined executable. MCP uses one binary, but each process selects exactly
one fixed `management`, `runtime`, or `observability` tool set. CLI and MCP tool
definitions come from the same semantic catalog.

The browser UI and its backend are maintained in the separate
[Ephemeral Sandbox Console](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-console)
repository. External adapters consume the operation contract, catalog, and
client at immutable revisions; they do not redefine core operation vocabulary
or expose gateway credentials to untrusted callers.

The compatibility unit for those three crates is one reachable, immutable core
commit. Consumers must take `sandbox-operation-contract`,
`sandbox-operation-catalog`, and `sandbox-operation-client` from the same Git
URL and exact revision, and must commit the resulting lockfile. Operation IDs,
request and response schemas, catalog feature sets, and gateway authentication
or discovery behavior are part of that client-facing boundary. A change to any
of them is coordinated by publishing the core commit first, updating every
consumer pin and lockfile together, and passing the consumer's live
compatibility gate before retiring its previous validated pin. Mixing revisions
across the three crates is unsupported.

Each sandbox record has a `daemon_http` endpoint separate from its authenticated
daemon RPC endpoint. The HTTP listener exposes only:

```text
GET  /health
ANY  /forward/shared/<port>/...
ANY  /forward/isolated=<workspace_id>/<port>/...
POST /files/list
```

`file_list` is the deliberate HTTP-only operation exception. Direct
`/files/read`, `/files/write`, `/files/edit`, `/files/blame`,
`/observability/*`, and `/export/*` requests return `404`. Use the relevant
management, runtime, or observability CLI/MCP set, or an authenticated external
adapter, for those operations.

The optional `file_list` JSON fields are `path`, `workspace_session_id`, and
`limit`. The limit must be at least 1 and is clamped to the daemon's fixed
`runtime.file.max_list_entries` safety cap. See
[daemon HTTP](daemon-http/README.md), including the
[host-access example](daemon-http/README.md#access-a-web-server-from-the-host),
for request and forwarding details.

## Contract owners

The adapter-neutral operation envelope is owned by
`crates/sandbox-operations/contract`; semantic declarations and routes are
owned by `crates/sandbox-operations/catalog`; the daemon JSON-line wire codec,
framing, authentication, limits, and readiness handshake are owned by
`crates/sandbox-protocol`. LayerStack manifest schema and CAS fixtures are owned
by `crates/sandbox-runtime/layerstack`.
