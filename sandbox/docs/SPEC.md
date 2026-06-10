# EphemeralOS Sandbox — Target Architecture Spec

Status: **proposed** (target state; supersedes the `eos-protocol`-centric layout).
Scope: the sandbox system only — the host-side API service, the host engine, the
in-container daemon, and the contract artifact that binds them. Client
implementations (agent-core or otherwise) are out of scope; they are defined
entirely by `contract/`.

---

## 1. Goals

1. **One entry point.** External callers reach exactly one socket, served by
   `eos-api`. The per-sandbox daemons are unreachable from outside the host.
2. **Complete isolation, loose coupling.** No compiled code is shared across
   the host/box boundary. The only shared artifact is `sandbox/contract/`
   (data + prose). Drift is caught by conformance tests, not by a shared crate.
3. **Client-first vocabulary.** The public op catalog is derived from what a
   caller needs (acquire a sandbox, use files/commands/isolation/plugins, end a
   run), not from the historical daemon inventory. Internal and operator ops
   exist but are not part of the public surface.

## 2. Components

```
caller (out of scope)
   │  UDS, newline-delimited JSON, one request per connection
   ▼
eos-api            (bin, host)   receive → gate → route → return. No fleet logic.
   │ in-process calls
   ▼
eos-sandbox-host   (lib, host)   owns and reaches sandboxes: registry, lifecycle,
   │                             docker, endpoint, forward, recovery.
   │  loopback TCP (docker-published port) + auth token; `docker exec` fallback
   ▼
eosd / eos-daemon  (bin+lib, in-container)   executes in-box ops: files (layer
                                 stack + OCC), command sessions (PTY), isolated
                                 workspaces, plugins (PPC), audit, checkpoint.
```

| Component | Kind | Job | Must never |
|---|---|---|---|
| `eos-api` | bin | decode envelope, enforce visibility, route by catalog, return response | contain fleet logic or per-op branches |
| `eos-sandbox-host` | lib | container lifecycle, registry, endpoint resolution, forwarding, recovery | parse op semantics beyond catalog metadata |
| `eosd` / `eos-daemon` | bin+lib | dispatch and execute the in-box op catalog | know about Docker, sandbox_ids, or the fleet |
| `contract/` | data | the protocol: op catalog, fixtures, prose | contain code |
| `eos-cas` | lib (in-box) | the two frozen content hashes + manifest/layer types | be depended on by host-side crates |

Dependency law: `eos-api → eos-sandbox-host → (std/tokio/serde only)`.
Host crates never depend on in-box crates; in-box crates never depend on host
crates. Both sides conform to `contract/` via tests.

## 3. Wire protocol

### 3.1 Client hop (caller → eos-api)

- Transport: Unix domain socket (path from `--listen`). Access control =
  filesystem permissions. No auth field on this hop.
- Framing: one UTF-8 compact-JSON object terminated by `\n` per connection;
  response is one JSON line, then the server half-closes.
- Request envelope:

```json
{"op":"sandbox.file.read","sandbox_id":"sb-…","invocation_id":"<uuid4hex>","args":{…}}
```

| Field | Required | Notes |
|---|---|---|
| `op` | yes | canonical name from `contract/ops.json` |
| `sandbox_id` | for daemon-bound ops | absent on `sandbox.acquire` / `sandbox.list` |
| `invocation_id` | yes | uuid4 hex; correlates cancellation/heartbeat |
| `args` | yes (may be `{}`) | op-specific |

- Response: for forwarded ops, the daemon's response verbatim. For host ops,
  a host-built object. Both carry `success: bool`.
- Error envelope (same shape as the daemon's):

```json
{"success":false,"error":{"kind":"…","message":"…","details":{…}}}
```

API-level error kinds (in addition to daemon kinds passed through):

| kind | Raised when |
|---|---|
| `forbidden` | op exists but `visibility != public` on this socket |
| `unknown_op` | op not in catalog |
| `unknown_sandbox` | `sandbox_id` not in registry |
| `sandbox_unavailable` | recovery exhausted (connect/respawn failed) |
| `uncertain_outcome` | mutating op sent, daemon outcome unknowable after a failure; NOT retried (see §6) |

### 3.2 Box hop (eos-sandbox-host → daemon)

Unchanged from the frozen daemon protocol (`contract/PROTOCOL.md`, distilled
from `docs/contract/01-wire-protocol.md`):

- Loopback TCP to the docker-published port; one request per connection;
  compact JSON + `\n`; response read to EOF.
- `_eos_daemon_auth_token` stamped top-level by the host (popped by the daemon
  before dispatch). AF_UNIX path inside the container carries no auth.
- `_eos_daemon_protocol_version` carried inside `args` (currently inert).
- Limits: `MAX_REQUEST_BYTES = 16 MiB`, request read timeout 30 s.
- `sandbox_id` is stripped before forwarding; the daemon envelope is
  byte-compatible with the frozen fixtures.
- Fallback transport: `docker exec <container> eosd daemon --client <socket>
  <payload>` (the daemon binary as its own thin client over its AF_UNIX socket).

## 4. Op catalog

Canonical grammar: `sandbox.<verb>` for host ops, `sandbox.<service>.<verb>`
for daemon ops, `plugin.<id>.<op>` for dynamic plugin ops. The `api.*` names
are legacy aliases, all daemon-bound. The token `v1` is dead: protocol
versioning lives in `args`/`ops.json`, never in names.

### 4.1 Host ops (`served_by: host`, `visibility: public`)

| Op | Effect |
|---|---|
| `sandbox.acquire` | provision container + daemon (see §5); returns `sandbox_id` |
| `sandbox.release` | destroy container, drop registry entry |
| `sandbox.status` | host view (container/endpoint/recovery state) + embedded daemon readiness |
| `sandbox.list` | enumerate registry |

### 4.2 Daemon ops (`served_by: daemon`, `visibility: public`)

| Service | Op | Legacy alias |
|---|---|---|
| file | `sandbox.file.read` | `api.v1.read_file` † |
| | `sandbox.file.write` | `api.v1.write_file` |
| | `sandbox.file.edit` | `api.v1.edit_file` |
| command | `sandbox.command.exec` | `api.v1.exec_command` |
| | `sandbox.command.poll` | `api.v1.command.read_progress` |
| | `sandbox.command.write_stdin` | `api.v1.write_stdin` |
| | `sandbox.command.cancel` | `api.v1.command.cancel` |
| | `sandbox.command.collect_completed` | `api.v1.command.collect_completed` |
| | `sandbox.command.count` | `api.v1.command_session_count` |
| isolation | `sandbox.isolation.enter` | `api.isolated_workspace.enter` |
| | `sandbox.isolation.exit` | `api.isolated_workspace.exit` |
| | `sandbox.isolation.status` | `api.isolated_workspace.status` |
| plugin | `sandbox.plugin.ensure` | `api.plugin.ensure` |
| | `sandbox.plugin.status` | `api.plugin.status` |
| | `plugin.<id>.<op>` (dynamic) | unchanged |
| run | `sandbox.run.end` | `api.v1.cancel_workspace_runs_by_caller_id` |
| call | `sandbox.call.heartbeat` | `api.v1.heartbeat` † |
| | `sandbox.call.cancel` | `api.v1.cancel` |
| | `sandbox.call.count` | `api.v1.inflight_count` |

† Alias is pinned by immutable golden fixtures and is **never** removed.
All other aliases may sunset after consumers migrate.

### 4.3 Non-public ops

| Visibility | Ops | Caller |
|---|---|---|
| `internal` | `sandbox.runtime.ready` (alias `api.runtime.ready`) | host recovery machine only |
| `operator` | `sandbox.checkpoint.{layer_metrics, ensure_base, build_base, commit_to_workspace, commit_to_git, binding}` · `sandbox.audit.{pull, snapshot, reset_floor}` · `sandbox.run.cancel_all` · `sandbox.isolation.list_open` | `eos-api admin <op>` CLI; never the client socket |
| `test` | `sandbox.isolation.test_reset` | test builds only |

### 4.4 `contract/ops.json` schema

```json
{
  "protocol_version": 1,
  "ops": [
    {
      "name": "sandbox.file.read",
      "aliases": ["api.v1.read_file"],
      "served_by": "daemon",          // "host" | "daemon"
      "visibility": "public",         // "public" | "operator" | "internal" | "test"
      "family": "Files",
      "mutates_state": false,
      "summary": "Read one file from the layer stack or isolated workspace."
    }
  ]
}
```

Generated by `eosd dump-ops` (host-op entries contributed by a static section
owned by `eos-api`), checked in, and reviewed like code. Arg/response JSON
schemas are optional per-op fields, added incrementally; fixtures cover the
hot paths first.

## 5. Lifecycle (host engine)

**Provision** (`sandbox.acquire`):

1. `docker run` with labels `eos.sandbox_id`, `eos.tcp_port`, `eos.created_by`.
2. `put_archive` the `eosd` binary and merged config into the container.
3. `docker exec -d eosd daemon --spawn --socket … --pid-file … --log-file …
   --tcp-host 0.0.0.0 --tcp-port <port> --auth-token <fresh random>`.
4. Resolve published port via `docker port` (retry ≤ 15 s).
5. Ready-gate: poll `sandbox.runtime.ready` until `ready: true` (bounded).
6. Insert registry record; return `sandbox_id`.

**Destroy** (`sandbox.release`): `docker rm -f`, drop record. No daemon-side
courtesy calls — container teardown *is* the cleanup.

**Registry**: in-memory map `sandbox_id → {container, endpoint, token, state}`.
On `eos-api` startup the registry is **rebuilt from docker labels**
(`docker ps --filter label=eos.sandbox_id`); tokens are recovered from a
host-private state dir keyed by sandbox_id. A host restart MUST NOT orphan
running sandboxes.

## 6. Recovery (normative)

For a forwarded request that fails:

```
connect refused/reset ─► invalidate cached endpoint ─► re-resolve ─► retry once
        │ still failing
        ▼
docker exec thin-client fallback (eosd daemon --client)
        │ still failing
        ▼
respawn daemon in-place (docker exec --spawn …) ─► ready-gate
        │
        ├─ op.mutates_state == false  ─► replay original request
        └─ op.mutates_state == true   ─► return error kind "uncertain_outcome"
```

Empty response on a mutating op fails closed (`uncertain_outcome`) — a write is
never replayed after an ambiguous outcome. Connect-retry backoff:
0.25 / 0.5 / 1.0 / 2.0 s, then one final attempt (inherited from the frozen
host behavior).

## 7. Routing and visibility (normative)

`eos-api` routes purely by catalog lookup:

```
visibility != public                  → forbidden            (client socket)
served_by == host                     → eos-sandbox-host call
served_by == daemon (incl. plugin.*)  → host::forward(sandbox_id, envelope)
op not in catalog                     → unknown_op
```

`eos-api` MUST NOT branch on specific op names; the only per-op data it reads
is `served_by`, `visibility`, and `mutates_state`.

## 8. File/folder structure

```
sandbox/
├── README.md                       entry point (NEW)
├── CONTRACT.md                     version-pin pointers (amended)
├── contract/                       shared artifact — data only (NEW)
│   ├── ops.json
│   ├── fixtures/*.json             moved from eos-protocol/fixtures/
│   └── PROTOCOL.md                 framing/envelope/auth/errors/canonicalization
├── crates/
│   ├── eos-api/                    NEW bin: main, server, wire, public, router, admin
│   │   └── tests/contract.rs
│   ├── eos-sandbox-host/           NEW lib: registry, lifecycle, docker, endpoint,
│   │   │                           forward, recovery   (promoted from eos-e2e-test)
│   │   └── tests/
│   ├── eosd/                       + dump-ops subcommand
│   ├── eos-daemon/
│   │   ├── src/wire/               absorbed: envelope, ops catalog (+aliases), errors, version
│   │   └── tests/contract.rs
│   ├── eos-cas/                    renamed rump of eos-protocol: cas.rs, models.rs,
│   │                               runner.rs (daemon↔ns-runner wire DTOs)
│   ├── eos-plugin/                 + framing.rs (own PPC framing)
│   ├── eos-workspace-runtime/      src/contract/ + ids.rs (absorbed typed IDs)
│   ├── eos-layerstack/ eos-occ/ eos-overlay/ eos-occ-layerstack/
│   │   eos-ns-child/               absorbed eos-runner + eos-ns-holder
│   │                               (holder + runner modules; DTOs → eos-cas)
│   ├── eos-e2e-test/               shrinks; imports eos-sandbox-host
│   └── eos-protocol/               DELETED (final step)
├── docs/
│   ├── README.md                   index (NEW)
│   ├── API.md                      GENERATED from ops.json
│   └── contract/ …                 frozen historical contracts (unchanged)
└── xtask/                          + check-contract, + gen-docs
```

## 9. Conformance (the drift defense)

`cargo xtask check-contract` is a REQUIRED CI gate:

1. `eosd dump-ops` output must equal the committed `contract/ops.json`.
2. Daemon conformance: decodes request fixtures byte-exactly; error envelopes
   match fixture shapes after the documented canonicalization (drop `timings`,
   `daemon_pid`, `uptime_s`).
3. Host conformance: `eos-sandbox-host` encodes requests that reproduce the
   request fixtures; `eos-api` refuses non-public ops; router covers every
   catalog entry.
4. Alias integrity: no alias collides with a canonical name; the two
   fixture-pinned aliases exist.

CAS byte-identity remains governed by `docs/contract/02-cas-byte-identity.md`
and the 18 golden cases — `eos-cas` carries them; host-side crates never
depend on it.

## 10. Migration plan (each phase ships green)

| Phase | Work | Verify |
|---|---|---|
| 1 | `contract/` (fixtures moved, PROTOCOL.md), `eosd dump-ops`, `ops.json`, `check-contract` gate — `eos-protocol` still present | gate passes; fixture tests repointed |
| 2 | `eos-sandbox-host` (promote e2e docker/container/client); repoint `eos-e2e-test` | e2e suite green against live daemon |
| 3 | fix `audit/events.rs` op-name literals → catalog consts; dispatcher alias table (canonical + legacy); `eos-api` + contract tests | e2e invokes ops under both spellings |
| 4 | absorb wire into `eos-daemon/src/wire/`; `eos-cas` rename; `eos-plugin` framing; ids → `eos-workspace-runtime::contract`; delete `eos-protocol` | workspace builds; `check-contract` green |
| 5 | READMEs, `docs/API.md` generation, gitignore hygiene | docs regenerate-and-diff clean |

## 11. Out of scope

Client implementations (generated or hand-written), warm pooling, multi-host
fleets, quotas/rate limits, remote operator access. All extend the host side
without changing this spec's component boundaries.
