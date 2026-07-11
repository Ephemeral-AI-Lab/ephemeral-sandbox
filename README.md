# EphemeralOS Sandbox

EphemeralOS separates adapter-neutral operation semantics, product adapters,
wire transport, host-side management, in-sandbox applications, and runtime
primitives.

```text
operator or agent
   | sandbox-manager-cli / sandbox-runtime-cli / sandbox-observability-cli
   | sandbox-mcp --set management|runtime|observability
   | sandbox-console
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

| Component | Kind | Job | Must never |
|---|---|---|---|
| `sandbox-operation-contract` | lib | own adapter-neutral operation, argument, scope, route, request, response, and application-error types | depend on any workspace package or own wire/presentation behavior |
| `sandbox-operation-catalog` | lib | own canonical internal identifiers and routes unconditionally, plus every public declaration and route in feature-gated manager/runtime/observability modules | depend on anything except the contract, own CLI metadata, or contain handlers |
| `sandbox-operation-client` | lib | own gateway discovery and wire transport shared by CLI, MCP, and console, plus value-based request construction shared by CLI and MCP | depend on the catalog, applications, adapters, or `sandbox-config` |
| `sandbox-gateway` | bin+lib | compose the public gateway listener, manager application, Docker provider, daemon wire client, and local daemon installer | own application behavior, depend on CLI/MCP/console or the shared client, or compose runtime applications directly |
| `sandbox-cli` | lib + 3 bins | own CLI paths, flags, positionals, help, output, and separately feature-gated manager/runtime/observability executables | depend on protocol/applications/other adapters, provide a combined executable, or let one binary enumerate another authority |
| `sandbox-mcp` | bin | project exactly one selected domain from the merged catalog as a stdio MCP server and send through the shared client | define a second catalog, expose a combined set, or depend on protocol/applications/CLI/console |
| `sandbox-console` | bin | serve the SPA, validate public `/api/rpc` routes, send through the shared client, and proxy the allowed per-sandbox daemon HTTP surface | define operation vocabulary, depend on protocol/applications/CLI/MCP, contact daemon RPC directly, or expose gateway credentials to the browser |
| `sandbox-manager` | lib | own sandbox lifecycle, daemon endpoint tracking, system-scoped operation handlers, routing, and application ports | depend on protocol/client/adapters/composition roots or implement runtime command/workspace semantics |
| `sandbox-protocol` | lib | own wire codec, framing, authentication fields, limits, and the daemon readiness handshake | own operation declarations/help or depend on catalog/applications/client/adapters |
| `sandbox-daemon` | bin+lib | compose authenticated RPC, the exact HTTP allowlist, runtime dispatch, observability dispatch, sampling, and lifecycle | depend on product adapters/client/manager or expose operation routes over HTTP beyond `file_list` |
| `sandbox-observability-query` | lib | own structured observability query selection and response construction through an application-owned input port | depend on protocol/client/adapters/daemon or the concrete runtime application |
| `sandbox-observability-telemetry` | lib | own tracing, events, sampling, collection, and reading primitives | depend on any workspace package |
| `sandbox-runtime` | lib | own public runtime handlers plus canonical internal workspace-session/layerstack dispatch and orchestration | depend on protocol/client/adapters/composition roots or own low-level runtime primitives |
| `sandbox-runtime-workspace` | lib | workspace runtime lifecycle, namespace handles, capture, and destroy | own command process state |
| `sandbox-runtime-layerstack` | lib | content hashes, manifest/layer types, storage, leases | own command execution |
| `sandbox-runtime-namespace-execution` | lib | namespace execution engine, PTY I/O, and transcript read/write windowing | own workspace lifecycle |
| `sandbox-runtime-namespace-process` | lib | namespace holder/runner bodies and setns execution | own operation dispatch |
| `sandbox-runtime-overlay` | lib | low-level overlay mount and unmount primitives | own workspace lifecycle |
| `sandbox-config` | lib | own sandbox YAML loading, merging, validation, and typed console/gateway/manager/daemon/observability/runner/runtime schemas | depend on any workspace package or own runtime behavior |
| `sandbox-provider-docker` | lib | implement manager ports with Docker and use protocol only for daemon readiness | own generic lifecycle/rollback, application handlers, client behavior, or depend on `sandbox-daemon` |

**Boundary law:** semantic and application-envelope vocabulary lives in
`crates/sandbox-operations/contract`; every public declaration, route, and
canonical internal identifier lives in `crates/sandbox-operations/catalog`;
shared gateway client behavior lives in `crates/sandbox-operations/client`;
CLI metadata lives only in `crates/sandbox-cli/src/projection`; and wire-only
codec, framing, authentication, limits, and readiness live in
`crates/sandbox-protocol`. Applications (`sandbox-manager`, `sandbox-runtime`,
and `sandbox-observability-query`) never depend on protocol, the client,
product adapters, composition roots, or each other's implementations. The
contract, config, telemetry, layerstack, and overlay packages
have no workspace dependencies; the catalog depends only on the contract,
protocol depends only on the contract, and the client depends only on contract
and protocol. CAS fixtures live with `sandbox-runtime-layerstack`.

Exactly three organizational namespace directories exist under `crates/`:
`sandbox-operations/`, `sandbox-observability/`, and `sandbox-runtime/`. They
are grouping directories only: none has a root `Cargo.toml`, Rust facade,
package identity, or re-export layer. The operation ownership migration
[specification](docs/obsidian/ephemeral-os/implementation_plan/operation-migration/spec.md#target-dependency-law)
contains the exhaustive allowed-edge table.

## The pieces

- `crates/sandbox-runtime/layerstack/tests/fixtures/` - runtime-owned CAS
  fixtures.
- `crates/sandbox-operations/` - grouping-only namespace for `contract/`,
  `catalog/`, and `client/`.
- `crates/sandbox-observability/` - grouping-only namespace for `telemetry/`
  (`sandbox-observability-telemetry`) and `query/`
  (`sandbox-observability-query`).
- `crates/sandbox-runtime/` - grouping-only namespace for `operation/`
  (`sandbox-runtime`), `workspace/`, `layerstack/`, `namespace-execution/`,
  `namespace-process/`, and `overlay/`.
- `crates/` - also contains the flat packages `sandbox-cli`, `sandbox-config`,
  `sandbox-console`, `sandbox-daemon`, `sandbox-gateway`, `sandbox-manager`,
  `sandbox-mcp`, `sandbox-protocol`, and `sandbox-provider-docker`.
- `e2e/` - maintained live operation E2E suite for CLI, MCP, console,
  gateway, manager, daemon, runtime, and observability behavior.
- `web/console/` - tracked SPA source; build output is staged under `dist/`.
- `config/prd.yml` - the single daemon config baseline (see `config/README.md`).
- `dist/` - packaged static `sandbox-daemon` binaries uploaded into sandbox
  containers.

## Common tasks

```sh
# expose repo-local sandbox tools for this shell
export PATH="$PWD/bin:$PATH"

# build all three CLI executables and the stdio MCP executable
cargo build -p sandbox-cli --all-features -p sandbox-mcp

# package the Docker daemon binary if needed and start/restart the public gateway
start-sandbox-docker-gateway

# bootstrap the whole web console stack (gateway + SPA build + console server)
start-sandbox-console-stack        # then open http://127.0.0.1:7880

# in another shell, use the gateway clients directly
sandbox-manager-cli list_sandboxes
sandbox-runtime-cli --sandbox-id eos-abc exec_command pwd
sandbox-observability-cli snapshot --sandbox-id eos-abc

# each help surface lists only its own catalog
sandbox-manager-cli help
sandbox-runtime-cli --sandbox-id eos-abc help
sandbox-observability-cli help

# one-time per machine: bootstrap the musl cross toolchain (zig + cargo-zigbuild)
setup-musl-cross

# package the in-container daemon binary for Docker/E2E iteration
# (builder auto-selected: zigbuild -> cross; override with --builder)
cargo run -p xtask -- package

# final fat-LTO package
cargo run -p xtask -- package --profile release

# run focused daemon checks
cargo test -p sandbox-runtime
cargo test -p sandbox-daemon
```

## MCP registrations

Register each authority independently. Replace `/absolute/path/to/ephemeral-os`
with this checkout's absolute path after building `sandbox-mcp` as shown
above. Do not register a combined server; none exists.

```json
{
  "mcpServers": {
    "ephemeral-os-management": {
      "command": "/absolute/path/to/ephemeral-os/target/debug/sandbox-mcp",
      "args": ["--set", "management"]
    },
    "ephemeral-os-runtime": {
      "command": "/absolute/path/to/ephemeral-os/target/debug/sandbox-mcp",
      "args": ["--set", "runtime"]
    },
    "ephemeral-os-observability": {
      "command": "/absolute/path/to/ephemeral-os/target/debug/sandbox-mcp",
      "args": ["--set", "observability"]
    }
  }
}
```

The server accepts exactly one fixed `--set management|runtime|observability`
and exposes only `initialize`, `notifications/initialized`, `ping`,
`tools/list`, and `tools/call`. Tool definitions come from the selected domain
of the same merged semantic catalog used by the CLI binaries.

## Web console transport

The web console does not invoke MCP servers or CLI executables. Browser
management, command, file read/write/edit/blame, and observability requests go
to the console server's `POST /api/rpc`; the server keeps gateway credentials
private and sends authenticated gateway RPC through
`sandbox-operation-client`. Only file listing, health, and application
forwarding use the limited daemon HTTP surface below.

## Daemon HTTP allowlist

Each sandbox record has a `daemon_http` endpoint separate from its
authenticated daemon RPC endpoint. The HTTP listener exposes only:

```text
GET  /health
ANY  /forward/shared/<port>/...
ANY  /forward/isolated=<workspace_id>/<port>/...
POST /files/list
```

`file_list` is the deliberate HTTP-only operation exception. Direct
`/files/read`, `/files/write`, `/files/edit`, `/files/blame`,
`/observability/*`, and `/export/*` requests return `404`. Use the relevant
management, runtime, or observability CLI/MCP set—or the console's
authenticated `/api/rpc` bridge—for those operations. See
[`docs/daemon-http/README.md`](docs/daemon-http/README.md) for request and
forwarding details.

The optional `file_list` JSON fields are `path`, `workspace_session_id`, and
`limit`. `limit` must be at least 1 and is always clamped to the daemon's fixed
`runtime.file.max_list_entries` safety cap.

## Contract owners

The adapter-neutral operation envelope is owned by
`crates/sandbox-operations/contract`; semantic declarations and routes are
owned by `crates/sandbox-operations/catalog`; the daemon JSON-line wire codec,
framing, authentication, limits, and readiness handshake are owned by
`crates/sandbox-protocol`.
LayerStack manifest schema and CAS fixtures are owned by
`crates/sandbox-runtime/layerstack`.
