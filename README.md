# EphemeralOS Sandbox

EphemeralOS is now centered on the in-container daemon and its runtime crates.
The former fleet manager and sandbox gateway crates have been removed.

```
RPC caller
   | newline-delimited JSON over eosd daemon transport
   v
eosd / daemon
   | dispatch_operation
   v
daemon_operation
   | command, workspace session, remount orchestration
   v
workspace / command / layerstack / namespace-process / overlay
```

| Component | Kind | Job | Must never |
|---|---|---|---|
| `eosd` / `daemon` | bin+lib | bind daemon transport and dispatch daemon requests | know about Docker fleets |
| `daemon_operation` | lib | command operation surface plus internal workspace session/remount orchestration | own low-level runtime primitives |
| `workspace` | lib | workspace runtime lifecycle, namespace handles, capture, destroy, remount | own command process state |
| `command` | lib | PTY, transcript, process, process-group primitives | own workspace lifecycle |
| `layerstack` | lib | content hashes, manifest/layer types, storage, leases, compaction | own command execution |

**Boundary law:** daemon transport vocabulary lives in
`crates/daemon/rpc_protocol`; daemon request dispatch lives in
`crates/daemon/server`; operation specs live in `crates/daemon/operation`; CAS
fixtures live with `layerstack`.

## The pieces

- `crates/daemon/layerstack/tests/fixtures/` - daemon-owned CAS fixtures.
- `crates/` - the workspace: `daemon/eosd`, `daemon/server`,
  `daemon/rpc_protocol`, `daemon/layerstack`, `daemon/overlay`,
  `daemon/namespace-process`, `daemon/command`, `daemon/operation`,
  `daemon/workspace`, and `daemon/config`.
- `config/prd.yml` — the single daemon config baseline (see `config/README.md`).
- `dist/` — packaged static `eosd` binaries uploaded into sandbox containers.

## Common tasks

```sh
# package the in-container daemon binary for Docker/E2E iteration
cargo run -p xtask -- package

# final fat-LTO package
cargo run -p xtask -- package --profile release

# run focused daemon checks
cargo test -p daemon_operation
cargo test -p daemon
```

## Contract owners

The shared daemon JSON-line RPC protocol is owned by `crates/daemon/rpc_protocol`.
LayerStack manifest schema and CAS fixtures are owned by `crates/daemon/layerstack`.
