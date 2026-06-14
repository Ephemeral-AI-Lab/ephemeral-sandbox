# EphemeralOS Sandbox

One host-side API process fronting a fleet of Docker sandboxes, each running
one in-container daemon. External callers reach exactly one socket; the
per-sandbox daemons are unreachable from outside the host. The full target
architecture is `docs/SPEC.md`.

```
caller
   │  UDS, newline-delimited JSON, one request per connection
   ▼
gateway (bin, host) receive → gate → route → return. No fleet logic.
   │ in-process calls
   ▼
host   (lib, host)   owns and reaches sandboxes: host engine,
   │                             protocol, runtime.
   │  loopback TCP (docker-published port) + auth token; `docker exec` fallback
   ▼
eosd / daemon  (bin+lib, in-container)   executes in-box ops: files (layer
                                 stack + OCC), commands (PTY), isolated
                                 workspaces, plugins (PPC), audit, checkpoint.
```

| Component | Kind | Job | Must never |
|---|---|---|---|
| `gateway` | bin | decode requests, enforce visibility, route by catalog, return response | contain fleet logic or per-op branches |
| `host` | lib | host engine, protocol client, Docker runtime | depend on daemon implementation crates |
| `eosd` / `daemon` | bin+lib | dispatch and execute the in-box op catalog | know about Docker, sandbox_ids, or the fleet |
| `crates/daemon/operation/ops.json` | data | reviewed static op catalog | drift from `eosd dump-ops` |
| `crates/shared/protocol/` | shared contract | op catalog, envelope/fault vocabulary, wire protocol prose and fixtures | depend on host/gateway/daemon implementation crates |
| `layerstack` | lib (in-box) | the two frozen content hashes + manifest/layer types, storage, leases, checkpoint squashing | be depended on by host-side crates |

**Boundary law:** host/gateway crates do not depend on daemon implementation
crates, and daemon crates do not depend on host/gateway crates. Cross-boundary
schemas live in `crates/shared/protocol` and `crates/shared/trace`; the
reviewed generated artifact is `crates/daemon/operation/ops.json`. Wire,
operation, and CAS fixtures live with their owning crates. `cargo run -p xtask
-- check-contract` is the drift gate.

## The pieces

- `crates/daemon/operation/ops.json` — the op catalog: canonical `host.*`
  names for host/fleet operations, canonical `sandbox.*` names for daemon
  operations, visibility, routing metadata, and protocol version.
- `crates/shared/protocol/PROTOCOL.md` — framing/auth/errors/canonicalization
  plus immutable wire fixtures in `crates/shared/protocol/fixtures/`.
- `crates/daemon/layerstack/tests/fixtures/` and
  `crates/daemon/operation/fixtures/` — daemon-owned CAS and operation
  fixtures.
- `crates/` — the workspace. Shared: `shared/protocol`, `shared/trace`.
  Gateway: `gateway`. Host: `host`. Daemon side:
  `daemon/eosd`, `daemon/core`, `daemon/layerstack`, `daemon/overlay`,
  `daemon/namespace`, `daemon/command`, `daemon/operation`,
  `daemon/plugin`, `daemon/workspace`, and `daemon/config`.
- `docs/API.md` — the public op reference, generated from
  `crates/daemon/operation/ops.json` (`cargo run -p xtask -- gen-docs`).
- `docs/contract/` — the frozen historical wire/CAS/audit contracts.
- `config/prd.yml` — the single daemon config baseline (see `config/README.md`).
- `dist/` — packaged static `eosd` binaries uploaded into sandbox containers.

## Common tasks

```sh
# the contract drift gate (CI-required)
cargo run -p xtask -- check-contract

# regenerate the catalog artifact and its rendered doc after editing
# protocol::catalog
cargo run -p eosd -- dump-ops > crates/daemon/operation/ops.json
cargo run -p xtask -- gen-docs

# package the in-container daemon binary (dist/eosd-linux-amd64)
cargo run -p xtask -- package

# live end-to-end suite against real Docker sandboxes
cargo run -p e2e-test --bin e2e-runner -- \
    --max-parallel 5 --container-weight-cap 10 --heavy-test-threads 4

# serve the sandbox gateway (one client socket + one operator socket beside it)
cargo run -p gateway -- serve --listen /tmp/eos-sandbox.sock \
    --image <docker-image> --platform linux/amd64
printf '%s\n' '{"op":"sandbox.checkpoint.layer_metrics","sandbox_id":"<sb-id>","invocation_id":"probe-1","args":{"layer_stack_root":"/eos/layer-stack"}}' \
    | socat - UNIX-CONNECT:/tmp/eos-sandbox.sock.operator
```

## Version pins

`CONTRACT.md` pins the wire protocol version and the on-disk manifest schema
version, and documents the bump procedure. Golden fixtures are immutable
ground truth — never regenerate them to match code.
