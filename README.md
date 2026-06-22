# EphemeralOS Sandbox

EphemeralOS is centered on a protocol boundary, host-side sandbox manager,
human-facing gateway CLI, in-sandbox daemon, and separated runtime packages.

```text
operator or agent
   | sandbox-cli or newline-delimited JSON protocol
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
   | command, workspace session, remount orchestration
   v
sandbox-runtime-command / sandbox-runtime-workspace /
sandbox-runtime-layerstack / sandbox-runtime-namespace-process /
sandbox-runtime-overlay / sandbox-runtime-config
```

| Component | Kind | Job | Must never |
|---|---|---|---|
| `sandbox-gateway` | bin+lib | own the public gateway listener and the `sandbox-cli` protocol client | own manager or runtime behavior |
| `sandbox-manager` | lib | own sandbox lifecycle, daemon endpoint tracking, and manager operations | implement runtime command/workspace semantics |
| `sandbox-protocol` | lib | own request/response DTOs, framing, catalog, and manual metadata | depend on manager, daemon, or runtime implementation crates |
| `sandbox-daemon` | bin+lib | bind daemon transport and dispatch runtime requests | know about Docker fleets |
| `sandbox-runtime` | lib | command operation surface plus internal workspace session/remount orchestration | own low-level runtime primitives |
| `sandbox-runtime-command` | lib | PTY, transcript, process, process-group primitives | own workspace lifecycle |
| `sandbox-runtime-workspace` | lib | workspace runtime lifecycle, namespace handles, capture, destroy, remount | own command process state |
| `sandbox-runtime-layerstack` | lib | content hashes, manifest/layer types, storage, leases, compaction | own command execution |
| `sandbox-runtime-namespace-process` | lib | namespace holder/runner bodies and setns execution | own operation dispatch |
| `sandbox-runtime-overlay` | lib | low-level overlay mount, move, and unmount primitives | own workspace lifecycle |
| `sandbox-runtime-config` | lib | runtime YAML loading, merging, and typed config schemas | own runtime behavior |

**Boundary law:** daemon transport vocabulary lives in
`crates/sandbox-protocol`; daemon request dispatch lives in
`crates/sandbox-daemon`; runtime operation dispatch and concrete operation specs
live in `crates/sandbox-runtime/operation`; CAS fixtures live with
`sandbox-runtime-layerstack`.

## The pieces

- `crates/sandbox-runtime/layerstack/tests/fixtures/` - runtime-owned CAS
  fixtures.
- `crates/` - the workspace: `sandbox-daemon`, `sandbox-protocol`,
  `sandbox-manager`, `sandbox-gateway`, `sandbox-runtime/operation`,
  `sandbox-runtime/command`, `sandbox-runtime/workspace`,
  `sandbox-runtime/namespace-process`, `sandbox-runtime/layerstack`,
  `sandbox-runtime/overlay`, and `sandbox-runtime/config`.
- `config/prd.yml` - the single daemon config baseline (see `config/README.md`).
- `dist/` - packaged static `sandbox-daemon` binaries uploaded into sandbox
  containers.

## Common tasks

```sh
# expose repo-local sandbox tools for this shell
export PATH="$PWD/bin:$PATH"

# start or restart the public gateway in the background
start-sandbox-gateway

# in another shell, use the gateway client directly
sandbox-cli manager list_sandboxes

# package the in-container daemon binary for Docker/E2E iteration
cargo run -p xtask -- package

# final fat-LTO package
cargo run -p xtask -- package --profile release

# run focused daemon checks
cargo test -p sandbox-runtime
cargo test -p sandbox-daemon
```

## Contract owners

The shared daemon JSON-line RPC protocol is owned by `crates/sandbox-protocol`.
LayerStack manifest schema and CAS fixtures are owned by
`crates/sandbox-runtime/layerstack`.
