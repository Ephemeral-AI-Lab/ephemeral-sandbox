# Phase 2 — Daemon process + lifecycle

**Estimated effort:** 6-7 days (4 days engineering + 2-3 days E2E)
**Risk profile:** HIGH — daemon process lifecycle is the canonical source of subtle bugs in this migration
**Status:** Not started
**Blocks on:** Phase 1 complete

## Goal

Spawn `python -m sandbox.code_intelligence.daemon` as a long-lived asyncio process inside the sandbox listening on `$HOME/.cache/eos-ci/<wh>/v1/daemon.sock`. Implement the wire protocol (length-prefixed msgpack frames), three control ops (`ping`, `shutdown`, `version`), the orchestrator-side daemon backend with retry-after-respawn semantics, and the launcher that handles spawn, health-check, and respawn-on-dead.

**Daemon spawn moves into the eager bootstrap hook from Phase 1.** `bootstrap_in_sandbox_ci_runtime` now: (1) uploads bundle (Phase 1), (2) spawns daemon and waits for socket readiness (Phase 2). This means by the time `create_sandbox` / `start_sandbox` returns, the daemon is reachable. `DaemonBackend._call_daemon_command`'s auto-respawn becomes a fallback for daemon-crash between calls — should rarely trigger in practice.

**No business logic moves in Phase 2.** Indexing from Phase 1 still runs (now via the daemon's `ping`-stub-then-replaced-in-Phase-3 path). Phase 2 is pure scaffolding around it. Phase 3 is when overlay/mutations/LSP move into the daemon.

## Why third

Three reasons:

1. **Process lifecycle is the riskiest single piece.** Async event-loop server, PID-file management, `kill -9` recovery, `dispose_sandbox` cleanup, `setsid` detachment, log redirection, socket polling — every one of these has subtle failure modes. Building it BEFORE moving business logic means failures here can't corrupt edits.
2. **Phase 1's bundle/launcher pattern generalizes to the daemon.** Phase 2 reuses the same `_runtime_bundle_bytes()` helper and adds `daemon/__main__.py` so the bundle is runnable as `python -m sandbox.code_intelligence.daemon` (the daemon entry).
3. **Provides the daemon command seam for Phase 3.** Phase 3 just adds methods to `server.py`'s dispatch table — the connection, framing, retry, and lifecycle are all already proven.

## What ships

| Artifact | File | Purpose |
|---|---|---|
| Daemon entrypoint | `backend/src/sandbox/code_intelligence/daemon/__main__.py` | `python -m sandbox.code_intelligence.daemon` launches the asyncio server |
| Daemon server | `backend/src/sandbox/code_intelligence/daemon/server.py` | asyncio Unix-socket server, daemon command dispatch, signal handling |
| Wire protocol | `backend/src/sandbox/code_intelligence/daemon/protocol.py` | Length-prefix + msgpack codec, schema version, error envelopes |
| daemon backend | `backend/src/sandbox/code_intelligence/backends/` | `DaemonBackend`: connect, frame, retry-after-respawn, `DaemonUnavailable` |
| Launcher | `backend/src/sandbox/code_intelligence/daemon/launcher.py` (extended from Phase 1) | `ensure_daemon(...)`: PID-file check, `kill -0` liveness, spawn, socket-readiness poll |
| Phase 2 live E2E | `backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py` | Spawn, ping, kill -9, respawn, dispose cleanup |
| Daemon unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_daemon_server.py` | Frame codec, dispatch, signal handling |
| Client unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_daemon_client_process_exec.py` | Retry semantics, `DaemonUnavailable`, frame round-trip |

## Detailed task list

### Task 2.1 — Wire protocol

**File to create:** `backend/src/sandbox/code_intelligence/daemon/protocol.py`

**Frame layout:** `[4-byte big-endian length][msgpack body]`. Max frame size: 64 MB (sanity ceiling; refuse larger frames).

**Body schema:**

```python
# Request
{
    "v": 1,             # schema version, int
    "id": str,          # client-chosen request id (uuid hex), echoed in response
    "op": str,          # method name (e.g. "ping", "shutdown", "version", and Phase 3 ops)
    "args": dict,       # method-specific arguments
}

# Response (success)
{
    "v": 1,
    "id": str,          # echoes request id
    "ok": True,
    "result": Any,      # method-specific
}

# Response (error)
{
    "v": 1,
    "id": str,
    "ok": False,
    "error": {
        "kind": str,    # "UnsupportedOp", "InvalidSchema", "InternalError", "OccAborted", "MergeConflict", ...
        "message": str,
        "details": dict # optional structured context
    }
}
```

**API:**

```python
import msgpack, struct
from dataclasses import dataclass

CI_PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 64 * 1024 * 1024

class FrameError(Exception): ...
class SchemaError(Exception): ...

@dataclass(frozen=True)
class CiRequest:
    id: str
    op: str
    args: dict

@dataclass(frozen=True)
class CiResponse:
    id: str
    ok: bool
    result: Any = None
    error: dict | None = None

def encode_frame(body: dict) -> bytes:
    payload = msgpack.packb(body, use_bin_type=True)
    if len(payload) > MAX_FRAME_BYTES:
        raise FrameError(f"frame too large: {len(payload)}")
    return struct.pack(">I", len(payload)) + payload

async def read_frame(reader: asyncio.StreamReader) -> dict:
    header = await reader.readexactly(4)
    (length,) = struct.unpack(">I", header)
    if length > MAX_FRAME_BYTES:
        raise FrameError(f"oversized frame header: {length}")
    body = await reader.readexactly(length)
    parsed = msgpack.unpackb(body, raw=False)
    if not isinstance(parsed, dict) or parsed.get("v") != CI_PROTOCOL_VERSION:
        raise SchemaError(f"bad schema version or shape: {parsed!r}")
    return parsed
```

**Verify:** unit test round-trips every error/result shape; rejects oversized frames; rejects bad schema versions.

### Task 2.2 — Daemon server

**File to create:** `backend/src/sandbox/code_intelligence/daemon/server.py`

**Entry signature:**

```python
async def run_daemon(workspace_root: str) -> None:
    """Start the asyncio Unix-socket server. Returns when the server stops."""
```

**Behavior:**

1. Resolve `state = storage.state_dir(workspace_root)` (creates the dir if needed; raises `StorageUnavailable` on failure — daemon exits with code 13).
2. Compute `socket_path = state / "daemon.sock"` and `pid_path = state / "daemon.pid"`.
3. **Stale-daemon check.** If `pid_path` exists, read PID; `os.kill(pid, 0)` — if alive, exit with code 11 ("daemon already running"). If dead, unlink stale `pid_path` and `socket_path`.
4. Write current PID to `pid_path`.
5. Start `asyncio.start_unix_server(handle_client, path=str(socket_path))`. Set socket file permissions to `0600` (owner-only).
6. Install SIGTERM/SIGINT handlers that trigger graceful shutdown:
   - Stop accepting new connections.
   - Wait up to 5s for in-flight requests to finish.
   - Force-close any stragglers.
   - Unlink `socket_path`, `pid_path`.
   - `sys.exit(0)`.
7. Run `await server.serve_forever()`.

**Dispatch table (Phase 2):**

```python
DISPATCH: dict[str, Callable[[dict], Awaitable[Any]]] = {
    "ping":     handle_ping,
    "shutdown": handle_shutdown,
    "version":  handle_version,
}

async def handle_ping(args: dict) -> dict:
    return {"pong": True, "uptime_s": time.time() - _started_at}

async def handle_shutdown(args: dict) -> dict:
    # Schedule graceful shutdown after replying
    asyncio.get_running_loop().call_later(0.05, lambda: os.kill(os.getpid(), signal.SIGTERM))
    return {"shutting_down": True}

async def handle_version(args: dict) -> dict:
    return {"protocol": CI_PROTOCOL_VERSION, "daemon": "0.1.0", "python": sys.version}
```

**Per-connection handler:**

```python
async def handle_client(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while not reader.at_eof():
            try:
                req = await read_frame(reader)
            except (FrameError, SchemaError, asyncio.IncompleteReadError):
                break
            handler = DISPATCH.get(req["op"])
            if handler is None:
                resp = {"v": 1, "id": req.get("id", ""), "ok": False,
                        "error": {"kind": "UnsupportedOp", "message": f"unknown op: {req['op']}"}}
            else:
                try:
                    result = await handler(req.get("args") or {})
                    resp = {"v": 1, "id": req["id"], "ok": True, "result": result}
                except Exception as exc:
                    resp = {"v": 1, "id": req["id"], "ok": False,
                            "error": {"kind": "InternalError", "message": str(exc),
                                      "details": {"traceback": traceback.format_exc()}}}
            writer.write(encode_frame(resp))
            await writer.drain()
    finally:
        writer.close()
        await writer.wait_closed()
```

**Logging:** All daemon stderr/stdout goes to `state / "daemon.log"`. Use Python `logging` with a `RotatingFileHandler` (10MB × 3 backups) writing to that path.

### Task 2.3 — Daemon entry

**File to create:** `backend/src/sandbox/code_intelligence/daemon/__main__.py`

```python
import argparse, asyncio, logging, sys
    from .server import run_daemon
from .storage import StorageUnavailable

def main() -> int:
    parser = argparse.ArgumentParser(prog="sandbox.code_intelligence.daemon")
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--log-level", default="INFO")
    args = parser.parse_args()

    logging.basicConfig(level=getattr(logging, args.log_level.upper(), logging.INFO))

    try:
        asyncio.run(run_daemon(args.workspace_root))
    except StorageUnavailable as exc:
        logging.error("storage unavailable: errno=%s path=%s message=%s",
                      exc.errno, exc.path, exc.message)
        return 13
    except KeyboardInterrupt:
        return 0
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

### Task 2.4 — daemon backend

**File to create:** `backend/src/sandbox/code_intelligence/backends/`

**API:**

```python
class DaemonUnavailable(Exception):
    """Daemon could not be reached even after ensure_daemon retry."""

class DaemonBackend:
    def __init__(self, transport: SandboxTransport, sandbox_id: str, workspace_root: str) -> None:
        self._transport = transport
        self._sandbox_id = sandbox_id
        self._workspace_root = workspace_root
        self._launcher = DaemonLauncher(transport, sandbox_id, workspace_root)
        self._home_cache: str | None = None

    async def call(self, op: str, args: dict | None = None, *, timeout: float = 30.0) -> Any:
        """Send one request, return the result. Raises DaemonUnavailable on connect failure
        even after ensure_daemon retry. Raises a typed exception for daemon-side error envelopes."""
        try:
            return await self._call_once(op, args or {}, timeout=timeout)
        except (ConnectionRefusedError, BrokenPipeError, FileNotFoundError, OSError):
            await self._launcher.ensure_daemon()
            try:
                return await self._call_once(op, args or {}, timeout=timeout)
            except (ConnectionRefusedError, BrokenPipeError, FileNotFoundError, OSError) as exc:
                raise DaemonUnavailable(f"daemon unreachable after respawn: {exc}") from exc

    async def _call_once(self, op: str, args: dict, *, timeout: float) -> Any:
        """One round-trip. Resolves $HOME if not cached, builds socket path, opens socket,
        sends frame, awaits frame, decodes."""
        ...
```

**Socket access via `transport.exec`:** The orchestrator cannot open a Unix socket inside the sandbox directly, so daemon command uses `transport.exec` to launch a short in-sandbox bridge. Phase 2 uses one of:

- **(A) `socat` shim** — `transport.exec(sandbox_id, f"socat - UNIX-CONNECT:{socket_path}", stdin=encoded_frame_bytes)`. Pros: well-established. Cons: requires `socat` in the sandbox image; per-call shell overhead.
- **(B) `nc -U` shim** — `transport.exec(sandbox_id, f"nc -U {socket_path}", stdin=encoded_frame_bytes)`. Pros: `nc` more universally available. Cons: `nc -U` flag varies across `nc` flavors (BSD vs GNU vs OpenBSD).
- **(C) Inline Python shim** — `transport.exec(sandbox_id, f"python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect({socket_path!r}); s.sendall(sys.stdin.buffer.read()); ... print(b64encode(...))'", stdin=...)`. Pros: zero external deps. Cons: more code in the shim.

**Recommendation: (C).** Python is guaranteed in the sandbox image (we already use it for the indexing CLI and overlay runtime); `socat`/`nc` aren't. Document the shim as `DaemonCommandClient._send_frame_via_process_exec()` in `daemon/client.py`. Phase 5 keeps the direct process.exec bridge unless a true provider-native persistent stream exists.

### Task 2.5 — Launcher (eager from `create_sandbox`, fallback on daemon command)

**File to extend:** `backend/src/sandbox/code_intelligence/daemon/launcher.py` (created in Phase 1; this phase adds daemon-spawn logic)

**Call sites (after Phase 2):**
- **Eager (primary):** `bootstrap_in_sandbox_ci_runtime` — called from `SandboxService.create_sandbox`, `start_sandbox`, and restart recovery. Daemon is up by the time these return.
- **Fallback (rare):** `DaemonBackend._call_daemon_command` retry path — covers daemon crash between calls. Should be a no-op in healthy runs.

**API:**

```python
class DaemonLauncher:
    def __init__(self, transport: SandboxTransport, sandbox_id: str, workspace_root: str) -> None: ...

    async def ensure_daemon(self, *, timeout_s: float = 10.0) -> None:
        """If daemon is alive, return. Otherwise upload the bundle and spawn it.
        Polls socket readiness up to timeout_s. Raises DaemonUnavailable on timeout.

        After Phase 2, called eagerly from bootstrap_in_sandbox_ci_runtime; the
        DaemonBackend retry path becomes a fallback for daemon-crash recovery."""

    async def is_alive(self) -> bool:
        """Read PID file via transport, kill -0 to check liveness."""

    async def spawn(self) -> None:
        """transport.exec the daemon launch command:
            cd /tmp/eos-ci-runtime && \
            setsid nohup python3 -m sandbox.code_intelligence.daemon \
                --workspace-root <root> \
                >$HOME/.cache/eos-ci/<wh>/v1/daemon.log 2>&1 </dev/null &
            echo $!
        Captures the PID; does NOT wait for the daemon — returns immediately so the
        caller can poll for socket readiness."""

    async def shutdown(self) -> None:
        """Send SIGTERM via transport.exec(kill -TERM $(cat <pid_file>)). Used by dispose."""
```

**Critical detail — `setsid`:** The daemon must outlive the `transport.exec` shell session. `setsid nohup ... </dev/null &` detaches it. Verify the spawn command literally produces a daemon whose parent is PID 1 (or the sandbox's init).

**Bundle reuse:** `ensure_daemon` calls `ensure_runtime_uploaded(transport, sandbox_id)` from Phase 1. The bundle is the same tar.gz; it just gains `__main__.py`. The bundle hash check (Task 1.4) prevents re-upload.

### Task 2.6 — Phase 2 live E2E

**File to create:** `backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py`

**Subtests:**

#### 2.6.A — Daemon ready immediately after `create_sandbox` (eager bootstrap)

```python
async def test_daemon_ready_after_create_sandbox(live_sweevo_env_factory):
    """KEY ASSERTION for the eager-bootstrap contract.
    By the time create_sandbox returns, ping must succeed without ensure_daemon retry."""
    h = TimingHarness(phase=2, test_name="daemon_ready_after_create")

    with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
        with h.step("create_sandbox_with_ci_bootstrap"):
            env = live_sweevo_env_factory()

    # No ensure_daemon retry should be needed — daemon already up
    with h.step("daemon_first_ping_no_retry"):
        backend = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.repo_dir, transport=env.transport)
        result = await backend._call_daemon_command("ping")
    assert result["pong"] is True

    with h.step("ps_aux_check"):
        code, out = env.exec("ps aux | grep sandbox.code_intelligence.daemon | grep -v grep | wc -l")
    assert int(out.strip()) == 1, f"expected exactly one daemon, got: {out}"

    # Eager bootstrap SLO
    cold = h.steps["create_sandbox_with_ci_bootstrap"]
    assert cold < 3.0, f"create_sandbox cold-start with eager CI > 3s ({cold:.2f}s)"

    # First ping should be fast (daemon already warm from create)
    first_ping = h.steps["daemon_first_ping_no_retry"]
    assert first_ping < 0.1, f"first ping > 100ms ({first_ping:.3f}s) — eager bootstrap may not have completed"

    print(h.report())
    print(h.compare_to(latest_phase0_baseline()))
    h.dump_json()
```

**Asserts:** `create_sandbox_with_ci_bootstrap` < 3s cold; `daemon_first_ping_no_retry` < 100ms (proves daemon was already up, no `ensure_daemon` retry needed).

#### 2.6.B — kill -9 + auto-respawn

```python
def test_daemon_kill_and_respawn(live_sweevo_env):
    h = TimingHarness(phase=2, test_name="kill_and_respawn")
    env = live_sweevo_env
    backend = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.repo_dir, transport=env.transport)

    with h.step("initial_spawn_and_ping"):
        await backend._call_daemon_command("ping")  # ensures daemon is up

    with h.step("daemon_kill9"):
        env.exec(f"kill -9 $(cat $HOME/.cache/eos-ci/{wh()}/v1/daemon.pid)")
        # Verify daemon is actually dead
        code, out = env.exec("ps aux | grep sandbox.code_intelligence.daemon | grep -v grep | wc -l")
        assert int(out.strip()) == 0

    with h.step("daemon_respawn_via_call"):
        result = await backend._call_daemon_command("ping")  # triggers ensure_daemon retry path
    assert result["pong"] is True

    with h.step("verify_new_pid"):
        code, out = env.exec("ps aux | grep sandbox.code_intelligence.daemon | grep -v grep | wc -l")
    assert int(out.strip()) == 1

    print(h.report())
    h.dump_json()
```

#### 2.6.C — Clean shutdown

```python
def test_daemon_clean_shutdown(live_sweevo_env):
    h = TimingHarness(phase=2, test_name="clean_shutdown")
    env = live_sweevo_env
    backend = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.repo_dir, transport=env.transport)

    with h.step("initial_spawn"):
        await backend._call_daemon_command("ping")

    with h.step("shutdown_daemon_command"):
        result = await backend._call_daemon_command("shutdown")
    assert result["shutting_down"] is True

    with h.step("post_shutdown_settle"):
        await asyncio.sleep(0.5)

    with h.step("verify_pid_cleanup"):
        code, _ = env.exec(f"test -f $HOME/.cache/eos-ci/{wh()}/v1/daemon.pid")
    assert code != 0, "PID file not cleaned up after shutdown"

    with h.step("verify_socket_cleanup"):
        code, _ = env.exec(f"test -S $HOME/.cache/eos-ci/{wh()}/v1/daemon.sock")
    assert code != 0, "Socket not cleaned up after shutdown"

    print(h.report())
    h.dump_json()
```

#### 2.6.D — dispose_sandbox cleanup

```python
def test_dispose_sandbox_no_orphan_daemon(live_sweevo_env_factory):
    h = TimingHarness(phase=2, test_name="dispose_no_orphan")

    with h.step("create_sandbox"):
        env = live_sweevo_env_factory()  # fresh sandbox

    with h.step("spawn_daemon"):
        backend = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.repo_dir, transport=env.transport)
        await backend._call_daemon_command("ping")

    with h.step("dispose_sandbox"):
        delete_test_sandbox(env.sandbox_id)

    # If sandbox was a real VM, it's gone — this assertion is trivially true.
    # If sandbox is a process group inside a shared host, verify no orphan.
    # Document the model: today Daytona = full-VM dispose, so daemon is implicitly killed.
    print(h.report())
    h.dump_json()
```

**Note:** Whether 2.6.D needs an explicit orphan-check depends on the sandbox isolation model. For Daytona's full-VM model, dispose tears down the entire process tree — assertion is trivially satisfied. Document this in the test docstring; if the sandbox model changes (e.g., shared-host containers), upgrade the assertion.

#### 2.6.E — Concurrent ping (correctness)

```python
async def test_concurrent_pings(live_sweevo_env):
    backend = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.repo_dir, transport=env.transport)
    results = await asyncio.gather(*[backend._call_daemon_command("ping") for _ in range(8)])
    assert all(r["pong"] is True for r in results)
```

This isn't a timing concern; it's a correctness check that the asyncio server handles concurrent connections. Phase 5 will measure concurrent throughput properly.

**Run command:** `uv run pytest backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py -m live -v -s`

### Task 2.7 — Unit tests

**`test_daemon_server.py`:**
- Frame round-trip: encode → decode produces identical dict.
- Oversized frame raises `FrameError`.
- Bad schema version raises `SchemaError`.
- Dispatch table includes `ping`, `shutdown`, `version`; unknown op produces `UnsupportedOp` error envelope.
- `handle_shutdown` schedules SIGTERM after replying.
- `handle_ping` returns `{"pong": True, "uptime_s": <number>}`.
- Stale PID file detection: write a PID file with PID 999999 (likely dead); daemon startup unlinks it and proceeds.
- Stale PID file with live PID: daemon exits with code 11.

**`test_daemon_client_process_exec.py`:**
- `call("ping")` returns the success-envelope `result` field.
- Connection failure triggers `ensure_daemon` then retries once.
- Second connection failure raises `DaemonUnavailable`.
- Daemon-side error envelope (`ok=False`) raises a typed exception with `kind`, `message`, `details`.
- Mock the transport — these are unit tests; no real sandbox.

### Task 2.8 — Regression check

- `.venv/bin/pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q` — green with flag off.
- Re-run Phase 0 + Phase 1 live E2Es — both green.
- Phase 1 E2E with daemon-launcher path enabled (since the launcher is now part of `ensure_initialized`) — symbol counts still match Phase 0 baseline.

## Definition of done

- [ ] `protocol.py` ships with the documented frame/schema; round-trip and rejection unit tests pass.
- [ ] `server.py` runs as `python -m sandbox.code_intelligence.daemon`, accepts connections on `$HOME/.cache/eos-ci/<wh>/v1/daemon.sock`, dispatches `ping`/`shutdown`/`version`.
- [ ] PID file + socket file cleanup on graceful shutdown.
- [ ] Stale PID detection on startup (live → exit 11; dead → unlink and continue).
- [ ] `DaemonBackend._call_daemon_command(...)` retries once after `ensure_daemon`; raises `DaemonUnavailable` on second failure.
- [ ] `DaemonLauncher.ensure_daemon()` spawns via `setsid nohup`, polls socket readiness, returns within 10s.
- [ ] **Phase 2 live E2E (all 4 subtests A-D) passes against `dask__dask_2023.3.2_2023.4.0`.**
- [ ] `daemon_spawn` < 2s, `daemon_first_ping` < 100ms warm (per E2E assertions).
- [ ] No orphan daemon process after `dispose_sandbox` (or trivially-satisfied if sandbox model is full-VM, documented in test).
- [ ] Regression check: Phase 0 + Phase 1 E2Es + full unit suite green.
- [ ] Daemon `daemon.log` accessible for debugging — paste a sample log into the PR description.

## Risk callouts (Phase 2 specific)

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | Daemon outlives orchestrator session because `setsid` failed silently | Explicit verification in 2.6.A that PPID is 1 (or sandbox init); `ps -o pid,ppid,cmd` post-spawn |
| **HIGH** | Race between `kill -9` and `ensure_daemon` retry — multiple daemon processes spawned | Stale PID detection + `kill -0` check before spawning; document the race window |
| **HIGH** | `socat`/`nc`/python shim is slow per-call (extra `transport.exec`) — masks real daemon command latency | Document the shim cost; Phase 5 keeps the direct process.exec bridge unless a true provider-native persistent stream exists |
| **MEDIUM** | Daemon log fills the disk over a long-lived sandbox | `RotatingFileHandler` (10MB × 3 backups) caps at 30MB |
| **MEDIUM** | `msgpack` not pre-installed in sandbox image | Verify in 2.6.A: `python3 -c "import msgpack"` runs clean. If missing, add to bundle (vendored) or document `pip install` step |
| **MEDIUM** | `asyncio.start_unix_server` permission issues on sandbox FS (e.g., socket on a tmpfs that doesn't support Unix sockets) | E2E 2.6.A would catch this — add explicit `test -S` check post-spawn |
| **LOW** | Daemon hangs on graceful shutdown waiting for in-flight requests | 5s timeout then force-close; document |
| **LOW** | Frame ID collision (uuid hex, 128 bits) | Practically impossible; document expected uniqueness |

## Hand-off to Phase 3

Phase 3 picks up with:
- A working daemon process accepting daemon commands over a Unix socket.
- A wire protocol with extension room for new ops.
- A daemon backend retry path that already handles transient failures.
- An `ensure_daemon` launcher Phase 3 can call from `DaemonBackend.apply_edit`, `write_file`, etc.
- Phase 0/1/2 E2Es as a regression suite — Phase 3 adds the OCC-invariant E2E on top.
