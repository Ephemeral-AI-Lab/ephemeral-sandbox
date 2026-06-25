# Cross-Platform Host Docker Runtime Spec

**Status:** proposed · **Scope:** `sandbox-gateway` (listener + `sandbox-cli`
client + backend wiring), `sandbox-manager` (lifecycle rollback + daemon
endpoint/client), `sandbox-config` (gateway addr + new Docker section), a **new**
`sandbox-provider-docker` crate, `bin/` tooling, and the `sandbox-e2e-live-test`
harness + oneshot matrix. No `sandbox-daemon` request-semantics change.

**Target model:** **Windows/macOS/Linux host orchestration, Linux container
runtime.** Host-side orchestration (`sandbox-gateway`, `sandbox-cli`, Docker
provider) is portable. Every sandbox is a Linux Docker container, and the
`sandbox-daemon` plus namespace/overlay/cgroup runtime are Linux binaries that
run inside that container.

**Image model:** use the requested normal Docker image. The live verification
target for this spec is `python:3.11-bookworm`.
There is no required pre-built custom runtime image in v1. The Docker provider
creates a stopped container from the requested image, then `install_daemon`
uploads the Linux `sandbox-daemon` artifact and config into that container during
`sandbox-cli manager create_sandbox`.

This document is **spec only** — do not implement while reading it; build to the
Acceptance Criteria (§11). It renders the approved plan
(`~/.claude/plans/indexed-questing-duckling.md`) in the repo's spec format.
Anchors (§12) were gathered against this working branch
(`consolidate-namespace-execution-types`), which has uncommitted churn —
**re-verify line numbers before editing**.

---

## 1. Context & Problem

`sandbox-cli manager create_sandbox --image python:3.11-bookworm --workspace-root <abs>`
fails today with `sandbox runtime is not configured`. The gateway hardcodes
`UnconfiguredRuntime` + `UnconfiguredDaemonInstaller` in `default_manager_services()`
(`crates/sandbox-gateway/src/gateway/main.rs:94-101`), and **no production
`SandboxRuntime` implementation exists** anywhere — only test fakes. Every create
reaches the stub and errors. This same black-box path is the only thing
`experiments/sandbox-cli-latency/run.py --manager-lifecycle` and the
`sandbox-e2e-live-test` suites drive, so they are blocked too.

Two structural problems must be solved to make the path work with
**Windows/macOS/Linux host orchestration**:

1. **Lifecycle leak.** `create_sandbox` dispatch
   (`crates/sandbox-manager/src/operation/impls/management/create_sandbox.rs:46-93`)
   transitions `Creating → Ready` **before** daemon install/start/check and has
   **no rollback**. A real runtime leaks containers and store records on any
   partial failure.
2. **Unix-socket-only transports.** Every host-facing transport is an
   `AF_UNIX` socket. tokio's `UnixListener`/`UnixStream` are `#[cfg(unix)]` — they
   do not build or run on Windows. And on **Docker Desktop (macOS/Windows)** the
   container runs inside a Linux VM, so a bind-mounted Unix socket created by the
   in-container daemon is **not host-connectable** (the socket inode does not
   round-trip the file-sharing layer; the listener lives in the VM).

---

## 2. Goals / Non-goals

**Goals.**
- A real Docker-backed `SandboxRuntime` + daemon installer so the black-box flow
  works: `create_sandbox → inspect_sandbox → runtime exec_command → destroy_sandbox`.
- A leak-free manager lifecycle (rollback on any post-create failure).
- The gateway, `sandbox-cli`, Docker provider, and gateway→daemon transport run
  on **macOS, Windows, and Linux hosts** by moving every host-facing transport
  to **TCP loopback**.
- The sandbox runtime remains a **Linux container runtime**; no native
  Windows/macOS sandbox runtime is promised.
- One host can manage many live sandboxes with deterministic labels, dynamic
  published ports, bounded create/destroy concurrency, and restart recovery.

**Non-goals (this effort).**
- TCP encryption/TLS (loopback only; per-sandbox daemon auth token; gateway auth
  token for CLI↔gateway).
- Auto-pulling images (v1 requires the image pre-pulled — §4.3).
- `docker exec`-based forwarding, image build, registry auth, multi-arch.
- Changing `sandbox-daemon` request semantics (it already serves TCP+auth — §4).
- A Windows host-process daemon (the daemon is Linux-only and always runs inside
  the Linux container; only the *host-side* orchestration becomes portable).

---

## 3. Architecture (all-TCP transport)

Three transports move from `AF_UNIX` to TCP `127.0.0.1:<port>`. The daemon's
*internal* Unix socket inside the container stays (Linux-only, unused by the host).

```
operator / sandbox-cli
   │  [1] TCP 127.0.0.1:<gw> + gateway auth token
   │      (was AF_UNIX /tmp/eos-gateway.sock)
   ▼
sandbox-gateway  ── default_manager_services() selects backend ──┐
   │  SandboxManagerRouter → ManagerServices                     │ bollard
   │                                                             ▼ (DOCKER_HOST /
   │  [2] TcpSandboxDaemonClient                          Docker Engine  local default:
   │      TCP 127.0.0.1:<published>  + auth token            create/start/   unix sock | npipe)
   │      (was AF_UNIX bind-mounted socket)                  inspect/remove
   ▼                                                             │
┌──────────────────────────────── Linux container ──────────────┼───────────────┐
│  sandbox-daemon  serve --tcp-host 0.0.0.0 --tcp-port 7000 ...  │  Cmd after install│
│  [3] TCP listener (auth: _sandbox_daemon_auth_token)  ◀── published 7000→127.0.0.1:0 │
│  install step: upload linux daemon binary + config; mount host workspace → /workspace │
│  Privileged + CgroupnsMode:private + Init                                         │
└───────────────────────────────────────────────────────────────────────────────┘
```

Multiple sandboxes are independent containers owned by one gateway instance:

```text
                              host machine
┌──────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│  sandbox-cli                                                                 │
│      │                                                                       │
│      │ TCP 127.0.0.1:<gateway-port> + gateway auth                            │
│      ▼                                                                       │
│  sandbox-gateway                                                             │
│      │                                                                       │
│      ├── SandboxManagerRouter                                                │
│      │       │                                                               │
│      │       ├── SandboxStore                                                │
│      │       │     ├── sb-001 -> eos-sb-001, 127.0.0.1:49153                 │
│      │       │     ├── sb-002 -> eos-sb-002, 127.0.0.1:49154                 │
│      │       │     └── sb-003 -> eos-sb-003, 127.0.0.1:49155                 │
│      │       │                                                               │
│      │       ├── DockerSandboxRuntime                                        │
│      │       │     ├── create stopped container                              │
│      │       │     └── remove container                                      │
│      │       │                                                               │
│      │       ├── DockerSandboxDaemonInstaller                                │
│      │       │     ├── upload daemon/config into stopped container            │
│      │       │     ├── start container                                        │
│      │       │     └── inspect published port                                 │
│      │       │                                                               │
│      │       └── TcpSandboxDaemonClient                                      │
│      │             ├── sb-001 -> 127.0.0.1:49153 + daemon token               │
│      │             ├── sb-002 -> 127.0.0.1:49154 + daemon token               │
│      │             └── sb-003 -> 127.0.0.1:49155 + daemon token               │
│      │                                                                       │
│      └── bollard / Docker Engine API                                         │
│              ├── create/start/inspect/remove eos-sb-001                       │
│              ├── create/start/inspect/remove eos-sb-002                       │
│              └── create/start/inspect/remove eos-sb-003                       │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘

                              Linux containers
┌──────────────────────────────┐  ┌──────────────────────────────┐
│ container eos-sb-001          │  │ container eos-sb-002          │
│ image: python:3.11-bookworm   │  │ image: python:3.11-bookworm   │
│ workspace: /workspace         │  │ workspace: /workspace         │
│ sandbox-daemon uploaded       │  │ sandbox-daemon uploaded       │
│ daemon TCP: 0.0.0.0:7000      │  │ daemon TCP: 0.0.0.0:7000      │
│ published: 127.0.0.1:49153    │  │ published: 127.0.0.1:49154    │
│ labels: eos.sandbox_id=sb-001 │  │ labels: eos.sandbox_id=sb-002 │
└──────────────────────────────┘  └──────────────────────────────┘

┌──────────────────────────────┐
│ container eos-sb-003          │
│ image: python:3.11-bookworm   │
│ workspace: /workspace         │
│ sandbox-daemon uploaded       │
│ daemon TCP: 0.0.0.0:7000      │
│ published: 127.0.0.1:49155    │
│ labels: eos.sandbox_id=sb-003 │
└──────────────────────────────┘
```

Per-sandbox creation flow:

```text
manager create_sandbox
  -> Docker create stopped container from requested normal image
  -> store record as Creating
  -> upload prebuilt musl sandbox-daemon + config into container
  -> Docker start container
  -> inspect Docker-assigned host port
  -> authenticated TCP readiness check
  -> store endpoint
  -> transition record to Ready
```

Gateway restart recovery:

```text
gateway startup
  -> list Docker containers with eos.gateway_instance_id=<this gateway>
  -> inspect labels and published ports
  -> rebuild SandboxStore records
  -> resume forwarding to each daemon over TCP
```

**Manager ports (unchanged seams; `sandbox-manager` stays generic):**
- `SandboxRuntime` (`runtime.rs`): `create_sandbox`/`destroy_sandbox`.
- `SandboxDaemonInstaller` (`daemon_install.rs`): `install`/`start`/`check`/`stop`.
- `SandboxDaemonClient` (`daemon_client.rs`): `invoke_with_timeout`.
`sandbox-provider-docker` implements the first two with bollard; the third becomes
a unified `TcpSandboxDaemonClient`.

**Responsibility split.** `sandbox-manager` remains the generic lifecycle
orchestrator. It owns manager CLI operations, request validation, sandbox records,
state transitions, lifecycle ordering, rollback, destroy flow, and forwarding to
the sandbox daemon. `sandbox-provider-docker` owns only Docker mechanics behind
the manager traits: create/remove containers, upload daemon assets, start
containers, inspect published ports/labels, recover existing containers, and map
Docker failures into `ManagerError`.

```text
sandbox-cli
  -> sandbox-gateway
    -> sandbox-manager
      -> SandboxRuntime trait          implemented by sandbox-provider-docker
      -> SandboxDaemonInstaller trait  implemented by sandbox-provider-docker
      -> SandboxDaemonClient           TcpSandboxDaemonClient
```

**Two enabling facts confirmed in live code (no daemon change needed):**
- **Daemon TCP + auth already exist.** `serve --tcp-host --tcp-port --auth-token`
  binds a TCP listener; for `is_tcp` connections the daemon strips/validates a JSON
  field `_sandbox_daemon_auth_token` (`sandbox-protocol/src/auth.rs:1`
  `DAEMON_AUTH_FIELD`; `sandbox-daemon/src/server/dispatch.rs` `strip_tcp_auth`;
  `serve.rs` rejects a TCP listener without a token). Same newline-delimited JSON
  framing, 16 MiB cap (`sandbox-protocol` `MAX_REQUEST_BYTES`), 30 s read timeout.
- **Gateway connection handling is transport-agnostic.**
  `SandboxGatewayServer::handle_connection<S: AsyncRead + AsyncWrite + Unpin>`
  (`crates/sandbox-gateway/src/gateway/connection.rs:10`) already works with a
  `TcpStream`; only the *listener* (`lifecycle.rs:14`) is Unix-specific.

---

## 4. Resolved Design Decisions (with evidence)

1. **bollard for Docker.** Cross-platform engine connection via
   `Docker::connect_with_local_defaults()` honoring `DOCKER_HOST` — Unix socket on
   macOS/Linux, **named pipe on Windows**. Declared once in
   `[workspace.dependencies]`, consumed `dep.workspace = true` (per `CLAUDE.md`).
   *Confirm bollard's Windows named-pipe feature flag.*
2. **New crate `sandbox-provider-docker`.** Docker is one physical-runtime backend;
   `README.md:31-32` forbids the gateway owning runtime behavior and keeps
   `sandbox-manager` generic. The provider depends on `sandbox-manager` (traits +
   `SandboxId/SandboxRecord/SandboxDaemonEndpoint/ManagerError`) and `sandbox-config`
   (§4.7); it must **not** depend on `sandbox-daemon` (`README.md:34` "must never
   know about Docker fleets").
3. **Pre-pulled image (v1).** Container-create returns a clear error if the image
   is absent. Avoids the streaming image-pull surface; auto-pull is a fast-follow.
4. **Windows/macOS/Linux host orchestration → all host transports TCP.** tokio
   `UnixListener`/`UnixStream` are `#[cfg(unix)]`; Windows host orchestration
   needs the gateway listener (`lifecycle.rs:14`), the `sandbox-cli` client
   (`cli/client.rs` `UnixStream::connect`), and the manager→daemon client
   (`daemon_client.rs:80`) all on TCP. This does **not** make
   `sandbox-daemon` or the namespace/overlay/cgroup runtime Windows-native;
   those remain Linux binaries inside Linux containers.
5. **TCP-only unified daemon endpoint.** `SandboxDaemonEndpoint { socket_path }` →
   `{ host, port, auth_token }`; one `TcpSandboxDaemonClient` (no enum, no dual
   transport). Both the Docker installer and the Linux-local installer return TCP
   endpoints.
6. **Runtime creates; installer installs and starts.** The Docker runtime creates
   a stopped base Linux container. During the same
   `sandbox-cli manager create_sandbox` operation, `install_daemon` uploads the
   Linux `sandbox-daemon` binary plus config into that stopped container via the
   Docker Engine API, `start_daemon` starts the container with the installed
   daemon as its foreground `Cmd`, and `check_daemon` gates `Ready`. This
   preserves the manager lifecycle contract: `create_sandbox` creates the
   physical sandbox, `install_daemon` installs daemon assets into it,
   `start_daemon` starts the daemon, `check_daemon` gates `Ready`, and rollback
   can remove the container on any post-create failure.
7. **Config schema in `sandbox-config`** (`README.md:41` — it owns typed config
   schemas). New `configs/manager.rs` `DockerRuntimeConfig`; an **optional** root
   section so existing `config/prd.yml` still loads and the daemon ignores it.
8. **Gateway backend selector defaults to `none`.** Preserves zero-config
   `bin/start-sandbox-gateway` dev (Unconfigured runtime); `--backend docker` opts
   in. The `TcpSandboxDaemonClient` is wired unconditionally.
9. **Async→sync bridge mirrors `daemon_client.rs:40-52`.** Manager traits are sync
   and run under `spawn_blocking` (where `Handle::try_current()` is `Ok`). Spawn a
   `std::thread`, build a tokio runtime inside it, construct the bollard client
   inside that runtime, `block_on`, `join`. Never reuse one bollard `Docker` handle
   across separate ephemeral runtimes (binds the hyper conn-pool to a dropped
   reactor).
10. **Per-sandbox daemon auth token injected per request.** The `TcpSandboxDaemonClient`
    serializes the `Request` to a JSON object and inserts
    `DAEMON_AUTH_FIELD = auth_token` before framing (the daemon removes it before
    dispatch). Token generated by the Docker runtime (uuid), carried on the
    container as a label so the installer recovers it via inspect.
11. **Container hardening: `Privileged + CgroupnsMode:"private" + Init:true`.**
    Required for the daemon's `unshare`/overlay `fsopen`/mount and cgroup-v2
    delegation; private cgroupns yields a writable `/sys/fs/cgroup`; `Init` reaps
    reparented children.
12. **Gateway TCP binds `127.0.0.1` only and requires a gateway auth token.**
    Moving CLI↔gateway from a `0600` Unix socket to loopback TCP removes the file
    permission boundary; v1 must require a gateway token supplied by env/flag and
    inserted into each CLI request. The token is not serialized in manager
    responses or reports.
13. **Host/container path mapping is explicit.** User-facing
    `--workspace-root` remains the host path. The Docker provider maps it to a
    Linux container path, default `/workspace`, and starts `sandbox-daemon` with
    `--workspace-root /workspace`. Do not assume a macOS/Windows host path is a
    valid path inside the Linux container.
14. **Multi-sandbox ownership is label-driven.** Every Docker container gets
    labels for sandbox id, gateway instance id, host/container workspace paths,
    daemon token, created-at timestamp, and cleanup policy. On gateway startup,
    the Docker provider rebuilds manager state for containers that match its
    gateway instance id.

---

## 5. Phases

Each phase compiles and tests green on its own. **Phase 1 is independent and
mergeable alone.** Phases 2 and 3 are independent transport work (either order);
Phase 4 depends on Phase 3's endpoint shape; Phase 5 wires it; Phase 6 verifies.

### 5.1 Phase 1 — Manager lifecycle rollback (no transport/Docker)

**Delivers.** Reordered `create_sandbox` so `Ready` is set only after the daemon
endpoint is valid, plus best-effort rollback on every post-create failure.
**Does not deliver.** Any transport or Docker change.

**File:** `crates/sandbox-manager/src/operation/impls/management/create_sandbox.rs`
— rewrite the `dispatch` body (`:62-92`) to:
1. `runtime.create_sandbox()` → `id` (error returns directly).
2. `store.create(id, workspace_root)` → record stays **`Creating`**; on error,
   `runtime.destroy_sandbox(&SandboxRecord::new(id, workspace_root, Creating))`
   (the untracked runtime sandbox) then return the error.
3. `install_daemon` → `start_daemon` → `check_daemon` (a private
   `provision_daemon(services, &record) -> Result<SandboxDaemonEndpoint, …>`); on
   any error, `rollback(services, &record)` then return.
4. `store.update_endpoint(&id, Some(endpoint))` then
   `store.transition_state(&id, Creating, Ready)`; on error, `rollback`.
5. Return `record_value(ready)`.

`rollback`: best-effort `stop_daemon` → `runtime.destroy_sandbox` → `store.remove`,
each ignoring its own error. Reuse existing store ops (`store.rs`
`create`/`update_endpoint`/`transition_state`/`remove`). No inline comments
(`CLAUDE.md`); the two helper fns carry the meaning.

**Tests:** extend `crates/sandbox-manager/tests/manager_core.rs` `FakeInstaller`
with per-stage failure injection (install/start/check). Per case assert:
`FakeRuntime` recorded one `destroy_sandbox` for the id, the store has no record,
and the response carries the originating error kind. Keep a happy-path test (record
ends `Ready` with endpoint; no destroy). The existing create/list/inspect/destroy
lifecycle test stays valid (final record shape unchanged).

**Verify:** `cargo test -p sandbox-manager` · `cargo clippy --all-targets` · `cargo fmt`.

### 5.2 Phase 2 — Gateway + `sandbox-cli` transport: Unix → TCP loopback

**Delivers.** Gateway listens on TCP and `sandbox-cli` connects over TCP; verifiable
while the runtime is still Unconfigured (`manager list_sandboxes` works).

| File | Change |
|---|---|
| `crates/sandbox-gateway/src/gateway/lifecycle.rs` | `UnixListener::bind` (`:14`) → `TcpListener::bind(addr)`; delete `set_socket_permissions` (`:77-88`), `remove_file_if_exists` (`:13,:95`) and the socket `remove_file` in `cleanup_paths` (`:92`); keep pid write (`:16`). `accept_until_shutdown` (`:35`) takes the TCP listener. `handle_connection` unchanged (generic). |
| `crates/sandbox-gateway/src/gateway/connection.rs` | Before `decode_request_value`, parse the request as a JSON object, validate and strip a new gateway auth field (for example `_sandbox_gateway_auth_token`), then decode the original protocol request. |
| `crates/sandbox-config/src/configs/gateway.rs` | `socket_path: PathBuf` → bind address; `DEFAULT_GATEWAY_SOCKET "/tmp/eos-gateway.sock"` → `127.0.0.1:7878`. Reuse `SANDBOX_GATEWAY_SOCKET` env name (value now `host:port`) to minimize churn; keep `pid_path`; add required gateway auth token config/env. |
| `crates/sandbox-gateway/src/gateway/main.rs` | `--gateway-socket` flag name kept, value parsed as `host:port`; `gateway_socket_path()` resolves an address; require/validate gateway auth token when serving TCP. |
| `crates/sandbox-gateway/src/cli/client.rs` | `UnixStream::connect` → `TcpStream::connect`; make `read_response_line` generic `<S: AsyncRead + Unpin>`; rename `socket_path`; inject gateway auth token into every request before framing. |
| `crates/sandbox-protocol/src/auth.rs` | Add a gateway auth field constant next to `DAEMON_AUTH_FIELD`; gateway and CLI share the constant and never expose the token in responses. |
| `crates/sandbox-config/src/configs/cli.rs` | default + type → `host:port`; validate as an address; resolve CLI gateway token from flag/env. |
| `crates/sandbox-gateway/src/cli/output.rs` | `--gateway-socket` global arg value is `host:port`. |
| `bin/start-sandbox-gateway` | default `127.0.0.1:7878`; drop `rm -f "$socket_path"` (keep pid handling); generate or require a gateway token and export the env used by `bin/sandbox-cli`. |
| `crates/sandbox-e2e-live-test/src/gateway.rs` | readiness probe `std::os::unix::net::UnixStream` → `std::net::TcpStream::connect`; drop the `socket.exists()` file check. `src/cli_client.rs`/`src/config.rs` already pass the value as a string — feed `127.0.0.1:7878`. |

**Security:** bind `127.0.0.1` only and require gateway auth (§4.12). **Verify:** `start-sandbox-gateway`;
`sandbox-cli manager list_sandboxes`; `cargo test -p sandbox-gateway -p sandbox-config`.

### 5.3 Phase 3 — Manager daemon transport: Unix → unified TCP endpoint

| File | Change |
|---|---|
| `crates/sandbox-manager/src/model.rs` | `SandboxDaemonEndpoint { socket_path: PathBuf }` (`:85-96`) → `{ host: String, port: u16, auth_token: String }`; update `new(...)`. |
| `crates/sandbox-manager/src/daemon_client.rs` | replace `UnixSandboxDaemonClient` (`:21-22`) with `TcpSandboxDaemonClient`. Keep the timeout + thread-hop (`:40-52`) and current-thread runtime (`:60-66`); swap `UnixStream::connect` (`:80`) for `TcpStream::connect((host, port))`; in `request_line` (`:134`) serialize the `Request` to a `serde_json::Value` object and insert `DAEMON_AUTH_FIELD = auth_token` before `push(b'\n')`. Update `lib.rs` re-export + `gateway/main.rs` import. |
| `…/operation/impls/management/mod.rs` | `endpoint_value` (`:97-101`) emits `{host, port}` (omit token). |
| `…/management/get_observability_tree.rs` | `daemon_value` (`:262-269`) emits `{host, port}` (redact token). |
| `crates/sandbox-manager/src/daemon_install.rs` | `LocalSandboxDaemonInstaller` (Linux host-process dev path): launch daemon with `--tcp-host 127.0.0.1 --tcp-port <free> --auth-token <token>` (free port via a transient `TcpListener` on `127.0.0.1:0`); `check_daemon` → TCP-connect/auth poll (not `socket_path.exists()`); drop socket-file create/remove; keep the `#[cfg(unix)]` pid-based stop and `#[cfg(not(unix))]` stubs. |
| forwarding/observability consumers | `router/forward.rs`, `get_observability_tree.rs` invoke paths treat the endpoint opaquely — **no change**. |

**Tests:** update endpoint constructors/helpers + `socket_path` assertions in
`tests/manager_core.rs`, `tests/manager_router.rs`, `tests/daemon_install.rs`,
`crates/sandbox-gateway/tests/gateway_server.rs`, and the E2E assertion in
`tests/manager/lifecycle/create_sandbox/returns_ready.rs` (`/daemon/socket_path`
→ `/daemon/host` + `/daemon/port`). **Verify:** `cargo test -p sandbox-manager -p sandbox-gateway`.

### 5.4 Phase 4 — New crate `sandbox-provider-docker` (bollard)

**Delivers.** Two manager-port impls over bollard; the runtime creates a stopped
Linux container, the installer uploads daemon assets into that container, and the
installer starts it so the daemon is reachable via a Docker-published TCP port.

- **`DockerSandboxRuntime: SandboxRuntime`**
  - `create_sandbox`: gen `id` + `auth_token` (uuid); create a **stopped**
    container from the requested normal base image (`request.image`; live tests
    use `python:3.11-bookworm`) with **Binds**:
    `request.workspace_root` mounted to `container_workspace_root` (default
    `/workspace`); **PortBindings**
    `7000/tcp → 127.0.0.1:0`; **`Privileged:true, CgroupnsMode:"private",
    Init:true`**; **Labels**: `eos.sandbox_id`, `eos.gateway_instance_id`,
    `eos.auth_token`, `eos.daemon_port`, `eos.host_workspace_root`,
    `eos.container_workspace_root`, `eos.created_at`, `eos.cleanup_policy`;
    **`Cmd`** = `<container-daemon-binary> serve --config-yaml <container-config> \
    --workspace-root <container-workspace> --socket <container-runtime.sock> \
    --pid-file <container-runtime.pid> --tcp-host 0.0.0.0 --tcp-port 7000 \
    --auth-token <tok> --sandbox-id <id>`. Return `id` = container name
    (`eos-<uuid>`). Do **not** build a custom image, install daemon assets, or
    start the container here.
  - `destroy_sandbox`: `remove_container(force)`.
  - `recover_sandboxes`: list containers with `eos.gateway_instance_id=<this gateway>`,
    inspect labels and published ports, and return `SandboxRecord`s with TCP
    endpoints so the gateway can rebuild manager state after restart.
- **`DockerSandboxDaemonInstaller: SandboxDaemonInstaller`**
  - `install_daemon`: validate the stopped container exists, then upload a tar
    archive with the Linux daemon binary, daemon config YAML, and any required
    parent directories to configured container paths using Docker Engine
    `put_archive`. This is the concrete "install" step and runs inside the
    manager `create_sandbox` flow after runtime creation and before container
    start.
  - `start_daemon`: `start_container(id)`, then `inspect_container(id)` →
    published host port for `7000/tcp` +
    `eos.auth_token` label → `SandboxDaemonEndpoint { host:"127.0.0.1", port, auth_token }`.
  - `check_daemon`: poll by sending one **authenticated** lightweight request and
    accepting any framed JSON response as ready (a bare TCP connect through Docker's
    port proxy is not a reliable readiness signal). Use a configurable Docker-grade
    deadline (`readiness_timeout_ms`, default ~15 s) — the daemon does cgroup
    discovery + dual-listener bind before it is reachable.
  - `stop_daemon`: best-effort `stop_container` with timeout; tolerate already-stopped
    or already-removed containers. `destroy_sandbox` remains responsible for removal.
  - **Install details + failure observability.** The uploaded daemon binary's tar
    entry sets mode `0755`. On `install_daemon`/`start_daemon`/`check_daemon` failure,
    capture container `State`/`ExitCode` + `logs` (bollard) into the `ManagerError`
    *before* rollback removes the container — the daemon's stderr (its only failure
    channel as the container `Cmd`) is otherwise lost.
- **Shared:** a `DockerEngine` (bollard client + typed config), an archive builder
  for daemon installation, and a free `daemon_launch_argv(config, record, token)`
  mirroring `daemon_install.rs:52-81`. Keep two types (SRP) — do not collapse.
- **Async→sync bridge:** §4.9. Map bollard errors → `ManagerError::{RuntimeFailed,
  DaemonInstallFailed}`.
- **Tests** (`tests/`): unit-test the pure `daemon_launch_argv` + config→argv mapping.
  Real Docker is Phase 6.

Workspace wiring: root `Cargo.toml` `members` += crate; `[workspace.dependencies]`
+= `bollard` and the path dep; `README.md` component table += the crate.

### 5.5 Phase 5 — Config schema + gateway backend selector

- `crates/sandbox-config/src/configs/manager.rs` (**new**): `DockerRuntimeConfig`
  { optional explicit Docker endpoint else local default, `daemon_binary_path`,
  `daemon_config_yaml_path`, `container_daemon_binary_path`,
  `container_daemon_config_yaml_path`, `default_image` (opt),
  `container_workspace_root` (default `/workspace`), `platform` (optional;
  required when using only an amd64 daemon artifact on arm64 hosts), `privileged`
  (default true), `daemon_port` (default 7000), `gateway_instance_id`,
  `max_active_sandboxes`, `max_concurrent_creates`, `max_concurrent_destroys`,
  `readiness_timeout_ms` (default 15000), optional per-container `memory_bytes` /
  `nano_cpus` caps }.
  Register in `configs/mod.rs`; validate paths in `configs/validate.rs`.
  Optional root section (§4.7).
- `crates/sandbox-gateway/src/gateway/main.rs`: add `--backend <none|docker>`
  (default `none`, env `EOS_GATEWAY_BACKEND`), `--config-yaml <PATH>`, and
  gateway-token resolution. `none` → current Unconfigured pair; `docker` → load
  the doc, read `DockerRuntimeConfig`, build `DockerSandboxRuntime` +
  `DockerSandboxDaemonInstaller`, call provider recovery, prepopulate
  `SandboxStore`, then serve; clear startup error if the section is missing/invalid.
- `config/prd.yml` (or a sibling gateway config) gains the Docker section for E2E.

**Verify:** `cargo build` · `cargo clippy --all-targets` · `cargo test -p sandbox-config`.

### 5.6 Phase 6 — Live E2E verification

Harness is **attach-only** (`src/bin/eos-e2e.rs`) — it connects to an externally
started, docker-wired gateway; no harness logic change beyond the Phase 2 TCP probe.

Prereqs: (1) run `cargo run -p xtask -- package --target <linux-musl-target>` on
the host before creating sandboxes, producing `dist/sandbox-daemon-linux-amd64`
or `dist/sandbox-daemon-linux-arm64`; do not build a custom sandbox image, because
the Docker provider uploads that host musl binary into each stopped sandbox
container during `create_sandbox`; (2) pre-pull `python:3.11-bookworm` on the Docker
host; (3) `start-sandbox-gateway --backend docker --config-yaml config/prd.yml`.

Order: (1) manual black-box smoke (create → inspect → `exec_command pwd` → destroy);
(2) manager lifecycle suite (`tests/manager/lifecycle/`); (3) runtime smoke
(`tests/runtime/command/exec_command/one_shot.rs`); (4) oneshot exec matrix — add
the 5 files under `tests/runtime/command/exec_command/oneshot/`
(`success_and_output.rs`, `failure_and_validation.rs`, `running_and_timeout.rs`,
`isolation_and_cleanup.rs`, `cgroup_performance.rs`) implementing OS-EXEC-001…012
per `oneshot/TEST_MATRIX.md` + `oneshot/IMPLEMENT_PROMPT.md` (`build.rs` auto-mounts;
record each call with `sb.record(&rec)`; compile-gate `cargo test -p
sandbox-e2e-live-test --no-run`); (5) `run.py --manager-lifecycle`.

**Cross-platform host validation:** Linux first, then macOS Docker Desktop
(proves published-port and host/container path mapping), then Windows Docker
Desktop/WSL2. Windows compile checks are for **host orchestration crates only**;
daemon/runtime crates are checked as Linux targets because they run inside Linux
containers.

---

## 6. New crate file/folder structure

`← NEW`, `△` edited, `[unchanged]`.

```text
crates/
  sandbox-provider-docker/                 ← NEW crate (Docker SandboxRuntime + installer)
    Cargo.toml                             ← NEW  deps: sandbox-manager, sandbox-config, bollard, tokio, uuid, thiserror
    src/
      lib.rs                               ← NEW  re-exports DockerSandboxRuntime, DockerSandboxDaemonInstaller, config
      engine.rs                            ← NEW  DockerEngine (bollard client + typed config) + bridge helper
      runtime.rs                           ← NEW  impl SandboxRuntime
      installer.rs                         ← NEW  impl SandboxDaemonInstaller
      launch.rs                            ← NEW  pure daemon_launch_argv(config, record, token)
    tests/
      launch.rs                            ← NEW  argv/config-mapping unit tests
  sandbox-manager/src/{model,daemon_client,daemon_install}.rs   △  (Phase 3)
  sandbox-manager/src/operation/impls/management/{mod,create_sandbox,get_observability_tree}.rs △ (Ph 1, 3)
  sandbox-gateway/src/gateway/{lifecycle,connection,main}.rs · src/cli/{client,output}.rs △ (Ph 2, 5)
  sandbox-config/src/configs/{gateway,cli,mod,validate}.rs △ · configs/manager.rs ← NEW (Ph 2, 5)
  sandbox-protocol/src/auth.rs              △  + gateway auth field constant
  sandbox-e2e-live-test/src/gateway.rs △ · tests/runtime/command/exec_command/oneshot/*.rs ← NEW (Ph 2, 6)
Cargo.toml                                 △  members += provider; [workspace.dependencies] += bollard + path dep
README.md                                  △  component table += sandbox-provider-docker
config/prd.yml                             △  + optional manager/docker section
bin/start-sandbox-gateway                  △  default addr; token env; drop socket-file rm
```

---

## 7. JSON shape change (daemon endpoint)

`record_value` (`management/mod.rs:88-101`) feeds `create_sandbox`, `inspect_sandbox`,
`list_sandboxes` responses; `daemon_value` (`get_observability_tree.rs:262-269`)
feeds the observability tree. Both currently emit `daemon: { "socket_path": … }`.

After Phase 3: `daemon: { "host": "127.0.0.1", "port": <u16> }` (the `auth_token`
is **never** serialized into responses). The E2E assertion in
`create_sandbox/returns_ready.rs` and the manager unit assertions move from
`/daemon/socket_path` to `/daemon/host` + `/daemon/port`.

---

## 8. Workspace_root visibility — OPEN (verified; resolve before live)

`--workspace-root` is a required user host path, bind-mounted to the container
`/workspace` (§4.13) and passed to the daemon as `--workspace-root /workspace`.
A code trace shows that, **as currently implemented, the user's files at that path
are not visible** to sandboxed commands:

- No code captures `workspace_root` into a layer — `create_workspace` takes a
  layer-stack snapshot + lease and never scans the workspace
  (`workspace/src/service/impls/create_workspace.rs:21-32`); layer paths come only
  from the manifest (`layerstack/src/service/support.rs:8-18`).
- The overlay is `move_mount`'d **on top of** `/workspace` with lowerdirs from
  `/eos/layer-stack` only (`overlay/src/kernel_mount.rs:128-150`;
  `namespace-process/src/runner/setns/mount_overlay.rs:31`), **shadowing** the
  bind-mounted files.
- One-shot `exec_command` (no `--workspace-session-id`) uses the same path via
  `WorkspaceProfile::HostCompatible` (`command/service/exec_command.rs:93-101` →
  `core.rs:126-133`). `pwd` returns `/workspace` (the mount point) but the contents
  are the overlay's, not the user's (`shell_exec/request.rs:37-69`).

**Why it gates this effort.** A required, user-supplied `--workspace-root` whose
contents don't appear makes a "working" sandbox empty — contradicting the apparent
intent. This is **daemon/`workspace`-crate behavior, not code this spec changes**,
but the Docker path is the first end-to-end flow of a user host dir, so it must be
settled at Phase 6 step 0, before the oneshot matrix is meaningful.

**Resolve among:** (a) confirm `WorkspaceProfile::HostCompatible` is *meant* to
expose the workspace/host fs (the name implies it; the trace did not confirm how its
snapshot is built — verify first; likely the real answer); (b) a capture step is
missing (scan `workspace_root` → layer → lowerdir) — a `workspace`-crate change,
out of scope here; (c) one-shot should bypass the overlay and run directly in
`/workspace`. Also confirm a **fresh sandbox** (empty layer stack) still yields a
valid overlay (≥1 lowerdir).

**Corollary — overlay on Docker Desktop is SAFE.** lowerdir/upper/work all live on
`/eos/*` (the container's native fs); only the overlay *mount target* is the
bind-mounted `/workspace`, so a virtiofs/9p-backed workspace works. Residual:
mounting on a virtiofs mountpoint + umount latency — validate, don't assume.

---

## 9. Cross-cutting risks & constraints

1. **Windows host orchestration is not fully run-testable locally** (macOS dev
   box). Achieve correctness by removing unix-only APIs from host crates
   (Phases 2–3), keeping Linux-only process/runtime code out of Windows host
   builds, and verifying on Windows CI with Docker Desktop/WSL2.
2. **Gateway TCP loopback** has no file-perm gate — bind `127.0.0.1` only and
   require a gateway auth token in v1.
3. **Host crate dependency hygiene** — `sandbox-gateway`, `sandbox-cli`, and
   `sandbox-provider-docker` must not depend on Linux-only runtime crates unless
   those dependencies are target-gated. Windows checks target the host package
   subset, not the whole workspace.
4. **Host/container path mapping** — host `workspace_root` may be `/Users/...` or
   `C:\...`; container `workspace_root` should be a Linux path such as `/workspace`.
   All daemon/runtime config inside the container must use container paths.
5. **Linux daemon artifact** — the container cannot run a macOS or Windows build
   of `sandbox-daemon`. If only `linux/amd64` is packaged, set Docker
   `platform=linux/amd64` explicitly on arm64 hosts.
6. **bollard Windows named-pipe** — confirm `connect_with_local_defaults()` feature
   flags on Windows.
7. **Container privileges** — without `Privileged + CgroupnsMode:private + Init` the
   daemon's overlay/namespace/cgroup-v2 work fails or cgroup samples degrade.
8. **`nftables` absent in `python:3.11-bookworm`** — isolated-network workspaces fail at
   `nft`; install it in the image or default the workspace network to Shared.
   Validate during the runtime smoke.
9. **Auth token in a Docker label** is readable by anyone with Docker access (already
   full control) and only guards the loopback port — acceptable; documented.
10. **Many-sandbox pressure** — Docker daemon limits, port allocation, and gateway
    create/destroy concurrency must be bounded. Tests should create N sandboxes,
    run one command in each, list/inspect them, and destroy all.
11. **Image pre-pulled** (v1) — create returns a clear error otherwise.

---

## 10. Verification commands

```sh
export PATH="$PWD/bin:$PATH"
# Phase 1
cargo test -p sandbox-manager
# Phases 2–3 (host transports)
cargo test -p sandbox-gateway -p sandbox-config -p sandbox-manager
start-sandbox-gateway && sandbox-cli manager list_sandboxes   # TCP front door
# Phases 4–5 (provider + wiring)
cargo build && cargo clippy --all-targets
cargo test -p sandbox-provider-docker
# Windows host-orchestration correctness (where a toolchain exists)
cargo check -p sandbox-config -p sandbox-manager -p sandbox-gateway -p sandbox-provider-docker \
  --target x86_64-pc-windows-msvc
# Linux container runtime correctness
cargo check -p sandbox-daemon -p sandbox-runtime --target x86_64-unknown-linux-gnu
# Phase 6 (Linux/macOS/Windows host + Linux container runtime)
# Host-local Linux musl artifact build; do not build Rust in the sandbox
# container and do not build a custom sandbox image.
cargo run -p xtask -- package --target aarch64-unknown-linux-musl --builder cargo --profile package-local
file dist/sandbox-daemon-linux-arm64   # should report "statically linked"
start-sandbox-gateway --backend docker --config-yaml config/prd.yml
sandbox-cli manager create_sandbox --image python:3.11-bookworm --workspace-root "$PWD/.eos-ws"
sandbox-cli manager inspect_sandbox --sandbox-id <id>
sandbox-cli runtime --sandbox-id <id> exec_command pwd
sandbox-cli manager destroy_sandbox --sandbox-id <id>
cargo test -p sandbox-e2e-live-test --no-run
cargo fmt --check && git diff --numstat
```

---

## 11. Acceptance criteria checklist

```text
- [ ] create_sandbox: a failure at install/start/check destroys the runtime sandbox
      and removes the store record (no leak); Ready is set only after check_daemon.
- [ ] Gateway listens on 127.0.0.1:<port>; sandbox-cli connects over TCP; no AF_UNIX
      in gateway/cli/manager-daemon transports; gateway auth is required.
      `manager list_sandboxes` works over TCP.
- [ ] SandboxDaemonEndpoint is {host,port,auth_token}; TcpSandboxDaemonClient injects
      DAEMON_AUTH_FIELD; responses expose {host,port} only (token never serialized).
- [ ] sandbox-provider-docker implements SandboxRuntime + SandboxDaemonInstaller via
      bollard; runtime creates a stopped Linux container; installer uploads daemon
      binary/config into that container, starts it, and returns a published TCP
      endpoint; provider does not depend on sandbox-daemon.
- [ ] Docker provider maps host workspace roots to a Linux container workspace path
      (default /workspace), and daemon/runtime config uses container paths.
- [ ] Docker labels support recovery and many sandboxes: gateway startup rebuilds
      records for its containers; N concurrent sandboxes can create/list/inspect/exec/destroy.
- [ ] Gateway --backend defaults to none (dev unchanged); --backend docker wires the
      provider from sandbox-config DockerRuntimeConfig.
- [ ] cargo build + cargo clippy --all-targets + cargo fmt --check clean; focused
      unit tests pass on macOS; host-crate `cargo check --target x86_64-pc-windows-msvc`
      passes where the toolchain is available; daemon/runtime Linux target check passes.
- [ ] Live (Linux + macOS Docker Desktop + Windows Docker Desktop/WSL2): black-box
      create→inspect→exec_command pwd→destroy succeeds; manager lifecycle +
      oneshot exec matrix suites pass.
- [ ] §8 RESOLVED: a user's --workspace-root files are visible to exec_command, or the
      intended HostCompatible semantics are documented and the oneshot matrix reflects them.
- [ ] Provider surfaces container State/logs in ManagerError on install/start/check failure.
- [ ] git diff --numstat actual LOC deltas reported.
```

---

## 12. Anchor ledger

Gathered during exploration of this branch; **re-verify before editing** (the
branch has uncommitted churn). ✓ = read directly while authoring this spec.

| Anchor | Fact | |
|---|---|---|
| `sandbox-gateway/src/gateway/main.rs:94-101` | `default_manager_services()` wires Unconfigured runtime/installer | ✓ |
| `…/gateway/main.rs:103-146` | `UnconfiguredRuntime` / `UnconfiguredDaemonInstaller` defs | ✓ |
| `…/gateway/lifecycle.rs:14` | `UnixListener::bind(&config.socket_path)` | ✓ |
| `…/gateway/lifecycle.rs:77-88` | `set_socket_permissions` cfg(unix) 0o600 | ✓ |
| `…/gateway/lifecycle.rs:90-101` | `cleanup_paths`/`remove_file_if_exists` socket files | ✓ |
| `…/gateway/connection.rs:10` | `handle_connection<S: AsyncRead+AsyncWrite+Unpin>` (transport-agnostic) | reported |
| `…/cli/client.rs` | `UnixStream::connect`; `read_response_line(stream: UnixStream)` | reported |
| `sandbox-manager/src/operation/impls/management/create_sandbox.rs:46-93` | dispatch: store.create→transition Ready→install/start/check; no rollback | ✓ |
| `…/management/mod.rs:88-101` | `record_value` + `endpoint_value` (`socket_path`) | ✓ |
| `…/management/get_observability_tree.rs:262-269` | `daemon_value` (`socket_path`) | reported |
| `sandbox-manager/src/store.rs:18-118` | `create`/`update_endpoint`/`transition_state`/`remove` | ✓ |
| `sandbox-manager/src/model.rs:84-96` | `SandboxDaemonEndpoint { socket_path }` + `new` | reported |
| `sandbox-manager/src/daemon_client.rs:21-52` | `UnixSandboxDaemonClient` + thread-hop bridge | ✓ |
| `sandbox-manager/src/daemon_client.rs:80,134-148` | `UnixStream::connect`; `request_line` framing (token inject point) | ✓ |
| `sandbox-manager/src/daemon_install.rs:52-144` | `LocalSandboxDaemonInstaller` launch_spec/install/start/check/stop | reported |
| `sandbox-manager/src/runtime.rs:1-23` | `SandboxRuntime` trait + Create types | reported |
| `sandbox-manager/src/daemon_install.rs:21-29` | `SandboxDaemonInstaller` trait | reported |
| `sandbox-protocol/src/auth.rs:1` | `DAEMON_AUTH_FIELD = "_sandbox_daemon_auth_token"` | ✓ |
| `sandbox-daemon/src/server/dispatch.rs` | `strip_tcp_auth` removes/validates the field for `is_tcp` | reported |
| `sandbox-daemon/src/serve.rs` | `--tcp-host/--tcp-port/--auth-token`; TCP requires a token | reported |
| `sandbox-config/src/configs/gateway.rs:1-30` | `GatewayConfig{socket_path,pid_path,max}`; default socket const | reported |
| `sandbox-config/src/configs/cli.rs` | `GatewayConfig{gateway_socket_path}`; default socket const | reported |
| `sandbox-e2e-live-test/src/gateway.rs:1-29` | `std::os::unix::net::UnixStream` readiness probe | reported |
| `sandbox-e2e-live-test/.../oneshot/{TEST_MATRIX,IMPLEMENT_PROMPT}.md` | 12-case matrix, 5 proposed files, build.rs auto-mount | ✓ |
| `Cargo.toml` workspace `members` + `[workspace.dependencies]` | add provider + bollard | ✓ |
| `README.md:29-41` | component table + boundary law | ✓ |
| `…/management/create_sandbox.rs:27-35` | `workspace_root` required `--workspace-root` (ArgKind::Path) | ✓ |
| `namespace-process/src/runner/setns/mount_overlay.rs:31` | `mount_overlay(&request.workspace_root, …)` (overlay target) | reported |
| `overlay/src/kernel_mount.rs:128-150` | lowerdir from layer_paths; upper/work from scratch; `move_mount` onto workspace_root | reported |
| `workspace/src/service/impls/create_workspace.rs:21-32` | snapshot+lease; **no** workspace_root capture | reported |
| `layerstack/src/service/support.rs:8-18` | layer_paths from manifest, not workspace_root | reported |
| `command/service/exec_command.rs:93-101` · `core.rs:126-133` | one-shot → `WorkspaceProfile::HostCompatible` (same overlay path) | reported |
| `namespace-process/src/runner/shell_exec/request.rs:37-69` | `shell_cwd` defaults to workspace_root | reported |

---

## 13. Deferred follow-ups

- Auto-pull missing images (streaming `POST /images/create`).
- `nftables` baked into the sandbox image (or a configurable default network mode).
- Owned long-lived provider runtime + handle (optimization over per-call bridge).
- Retire `LocalSandboxDaemonInstaller` if the Docker backend fully supersedes the
  Linux host-process dev path.
