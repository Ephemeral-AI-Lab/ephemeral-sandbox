# EphemeralOS Sandbox

One host-side API process fronting a fleet of Docker sandboxes, each running
one in-container daemon. External callers reach exactly one socket; the
per-sandbox daemons are unreachable from outside the host. The full target
architecture is `docs/SPEC.md`.

```
caller
   │  UDS, newline-delimited JSON, one request per connection
   ▼
eos-api            (bin, host)   receive → gate → route → return. No fleet logic.
   │ in-process calls
   ▼
eos-sandbox-host   (lib, host)   owns and reaches sandboxes: host engine,
   │                             protocol, runtime.
   │  loopback TCP (docker-published port) + auth token; `docker exec` fallback
   ▼
eosd / eos-daemon  (bin+lib, in-container)   executes in-box ops: files (layer
                                 stack + OCC), command sessions (PTY), isolated
                                 workspaces, plugins (PPC), audit, checkpoint.
```

| Component | Kind | Job | Must never |
|---|---|---|---|
| `eos-api` | bin | decode envelope, enforce visibility, route by catalog, return response | contain fleet logic or per-op branches |
| `eos-sandbox-host` | lib | host engine, duplicated protocol client, Docker runtime | depend on a workspace-internal crate |
| `eosd` / `eos-daemon` | bin+lib | dispatch and execute the in-box op catalog | know about Docker, sandbox_ids, or the fleet |
| `contract/` | data | the protocol: op catalog, fixtures, prose | contain code |
| `eos-layerstack` | lib (in-box) | the two frozen content hashes + manifest/layer types, storage, leases, checkpoint squashing | be depended on by host-side crates |

**Isolation law:** no compiled code is shared across the host/box boundary.
The only shared artifact is `contract/` (data + prose); both sides prove
conformance against it, and `cargo run -p xtask -- check-contract` is the
drift gate.

## The pieces

- `contract/` — `ops.json` (the op catalog: canonical `sandbox.*` names,
  visibility, routing metadata), `PROTOCOL.md`
  (framing/auth/errors/canonicalization), and the immutable golden fixtures.
- `crates/` — the workspace. Host side: `eos-api`, `eos-sandbox-host`. Box
  side: `eosd` (binary), `eos-daemon` (server + `wire/` protocol),
  `eos-layerstack`, `eos-overlay`, `eos-namespace`, `eos-command-session`,
  `eos-command-ops`, `eos-ephemeral-workspace`, `eos-isolated-workspace`,
  `eos-file-ops`, and `eos-plugin`.
- `docs/API.md` — the public op reference, generated from `contract/ops.json`
  (`cargo run -p xtask -- gen-docs`).
- `docs/contract/` — the frozen historical wire/CAS/audit contracts.
- `config/prd.yml` — the single daemon config baseline (see `config/README.md`).
- `dist/` — packaged static `eosd` binaries uploaded into sandbox containers.

## Common tasks

```sh
# the contract drift gate (CI-required)
cargo run -p xtask -- check-contract

# regenerate the catalog artifact and its rendered doc after editing the
# catalog (crates/eos-daemon/src/wire/ops.rs)
cargo run -p eosd -- dump-ops > contract/ops.json
cargo run -p xtask -- gen-docs

# package the in-container daemon binary (dist/eosd-linux-amd64)
cargo run -p xtask -- package

# live end-to-end suite against real Docker sandboxes
cargo test -p eos-e2e-test --features e2e

# serve the sandbox API (one client socket + one operator socket beside it)
cargo run -p eos-api -- serve --listen /tmp/eos-api.sock \
    --image <docker-image> --platform linux/amd64
cargo run -p eos-api -- admin sandbox.checkpoint.layer_metrics \
    --listen /tmp/eos-api.sock --sandbox <sb-id> --args '{"layer_stack_root":"/eos/layer-stack"}'
```

## Version pins

`CONTRACT.md` pins the wire protocol version and the on-disk manifest schema
version, and documents the bump procedure. The golden fixtures under
`contract/fixtures/` are immutable ground truth — never regenerate them to
match code.
