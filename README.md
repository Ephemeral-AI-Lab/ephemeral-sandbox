# EphemeralOS Sandbox

EphemeralOS is centered on a protocol boundary, host-side sandbox manager,
human-facing gateway CLI, in-sandbox daemon, and separated runtime packages.

```text
operator or agent
   | sandbox-manager-cli / sandbox-runtime-cli / sandbox-observability-cli
   | sandbox-mcp --set management|runtime|observability
   | or authenticated newline-delimited JSON protocol
   v
sandbox-gateway / sandbox-protocol
   v
sandbox-manager
   | forwards sandbox-scoped runtime requests
   v
sandbox-daemon
   | dispatch_operation
   v
sandbox-runtime
   | command operations and workspace session orchestration
   v
sandbox-runtime-workspace / sandbox-runtime-layerstack /
sandbox-runtime-namespace-execution / sandbox-runtime-namespace-process /
sandbox-runtime-overlay
   |
   v
sandbox-config
```

| Component | Kind | Job | Must never |
|---|---|---|---|
| `sandbox-gateway` | bin+lib | own the public gateway listener | own manager or runtime behavior or any CLI client code |
| `sandbox-cli` | lib + 3 bins | provide feature-free `sandbox_cli::core` plus separately feature-gated management, runtime, and observability executables | provide a combined executable/set or let one binary enumerate another authority |
| `sandbox-mcp` | bin | project exactly one selected management, runtime, or observability catalog as a stdio MCP server | define a second operation registry, expose a combined set, or accept caller-supplied authority |
| `sandbox-console` | bin | web console: serve the SPA and bridge the browser to the gateway protocol (`/api/rpc`) and the allowed per-sandbox `daemon_http` surface (`/api/sandboxes/:id/health`, exact `/api/sandboxes/:id/files/list`, `/s/:id` preview proxy) as a client peer over `sandbox_cli::core` | define operation vocabulary, contact the daemon RPC endpoint directly, or expose the gateway auth token to the browser |
| `sandbox-manager-operations` | lib | manager CLI operation specs and catalog (spec-only) | contain dispatch or service code |
| `sandbox-runtime-operations` | lib | runtime CLI operation specs and catalog (spec-only) | contain dispatch or service code |
| `sandbox-observability-operations` | lib | observability CLI operation specs and catalog (spec-only) | contain dispatch or service code |
| `sandbox-manager` | lib | own sandbox lifecycle, daemon endpoint tracking, and manager operations | implement runtime command/workspace semantics |
| `sandbox-protocol` | lib | own request/response DTOs, framing, catalog, and help metadata | depend on manager, daemon, or runtime implementation crates |
| `sandbox-daemon` | bin+lib | bind authenticated RPC plus the exact HTTP allowlist (`GET /health`, `/forward/...`, and `POST /files/list`) and dispatch runtime requests | expose management/runtime/observability operation routes over HTTP or know about Docker fleets |
| `sandbox-runtime` | lib | command operation surface plus internal workspace session orchestration | own low-level runtime primitives |
| `sandbox-runtime-workspace` | lib | workspace runtime lifecycle, namespace handles, capture, and destroy | own command process state |
| `sandbox-runtime-layerstack` | lib | content hashes, manifest/layer types, storage, leases | own command execution |
| `sandbox-runtime-namespace-execution` | lib | namespace execution engine, PTY I/O, and transcript read/write windowing | own workspace lifecycle |
| `sandbox-runtime-namespace-process` | lib | namespace holder/runner bodies and setns execution | own operation dispatch |
| `sandbox-runtime-overlay` | lib | low-level overlay mount and unmount primitives | own workspace lifecycle |
| `sandbox-config` | lib | sandbox YAML loading, merging, and typed gateway/manager/CLI/daemon/runtime config schemas | own runtime behavior |
| `sandbox-provider-docker` | lib | implement the Docker-backed `SandboxRuntime` and `SandboxDaemonInstaller` behind the manager provider traits using the Docker Engine API (bollard) | own generic lifecycle/rollback or depend on `sandbox-daemon` |

**Boundary law:** daemon transport vocabulary lives in
`crates/sandbox-protocol`; daemon request dispatch lives in
`crates/sandbox-daemon`; runtime operation dispatch lives in
`crates/sandbox-runtime/operation`; CLI operation specs (spec-only) live in
`crates/sandbox-manager-operations`, `crates/sandbox-runtime-operations`, and
`crates/sandbox-observability-operations`;
CAS fixtures live with `sandbox-runtime-layerstack`.

## The pieces

- `crates/sandbox-runtime/layerstack/tests/fixtures/` - runtime-owned CAS
  fixtures.
- `crates/` - the workspace: `sandbox-daemon`, `sandbox-protocol`,
  `sandbox-manager`, `sandbox-gateway`, `sandbox-cli`, `sandbox-mcp`,
  `sandbox-console`,
  `sandbox-manager-operations`, `sandbox-runtime-operations`,
  `sandbox-observability-operations`, `sandbox-runtime/operation`,
  `sandbox-runtime/workspace`, `sandbox-runtime/namespace-execution`,
  `sandbox-runtime/namespace-process`, `sandbox-runtime/layerstack`,
  `sandbox-runtime/overlay`, and `sandbox-config`.
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
`tools/list`, and `tools/call`. Tool definitions come from the same three
canonical operation catalogs used by the CLI binaries.

## Web console transport

The web console does not invoke MCP servers or CLI executables. Browser
management, command, file read/write/edit/blame, and observability requests go
to the console server's `POST /api/rpc`; the server keeps gateway credentials
private and sends authenticated gateway RPC through `sandbox_cli::core`.
Only file listing, health, and application forwarding use the limited daemon
HTTP surface below.

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

## Contract owners

The shared daemon JSON-line RPC protocol is owned by `crates/sandbox-protocol`.
LayerStack manifest schema and CAS fixtures are owned by
`crates/sandbox-runtime/layerstack`.
