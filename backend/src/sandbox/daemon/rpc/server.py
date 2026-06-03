"""AF_UNIX/TCP server for the resident in-sandbox daemon.

Replaces the per-call ``python -m sandbox.daemon.rpc.dispatcher <json>`` boot path
with a single long-lived process that listens on AF_UNIX. Each host call
still goes through ``provider.exec(...)`` (Daytona constraint), but the
per-call command is now a thin client that connects to the socket, sends
one newline-terminated JSON envelope, and prints the JSON response. Docker
sandboxes may also expose the same daemon over a loopback-published TCP port
to avoid a per-tool ``docker exec`` hop.

Wire format (newline-delimited JSON):

  request:  {"op": "...", "invocation_id": "...", "args": {...}}\\n
  response: {"success": true, ...}\\n

The daemon imports :mod:`sandbox.daemon.rpc.dispatcher` so the ``OP_TABLE`` is
populated by the standard peer bootstrap, then dispatches via
:func:`dispatcher.dispatch_envelope_async`. State that is expensive to
construct — ``LayerStack``, ``Service``,
``SnapshotGitignoreOracle`` — is cached across calls by
``sandbox.daemon.occ_runtime_services`` and thus amortizes naturally because the daemon
is one Python process.

Lifecycle:

* The daemon is launched once per sandbox via
  ``sandbox.host.daemon_client`` issuing a ``nohup`` invocation
  through the provider adapter's ``exec``.
* It writes its PID to ``<bundle>/runtime.pid`` and binds AF_UNIX to
  ``<bundle>/runtime.sock``.
* Restart safety: stale PID and stale socket are cleaned up before bind.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import signal
from pathlib import Path

from sandbox.daemon.paths import DAEMON_PID_PATH, DAEMON_SOCKET_PATH
from sandbox.daemon.rpc import dispatcher
from sandbox._shared.clock import monotonic_now

logger = logging.getLogger("sandbox.daemon.rpc.server")

DEFAULT_SOCKET_PATH = DAEMON_SOCKET_PATH
DEFAULT_PID_PATH = DAEMON_PID_PATH
DAEMON_AUTH_FIELD = "_eos_daemon_auth_token"

# Cap a single request envelope to bound the daemon's per-connection memory and
# convert oversize payloads into a structured ``request_too_large`` error rather
# than a silent connection drop. ``api.write_file`` is the largest legitimate
# producer; 16 MiB leaves comfortable headroom over realistic source files.
MAX_REQUEST_BYTES = 16 * 1024 * 1024
# Bound the time a single ``readline`` may pin a connection task. Long enough
# for slow but legitimate clients; short enough to defang slowloris-style
# half-open peers from a buggy host.
REQUEST_READ_TIMEOUT_S = 30.0


async def _handle_connection(
    reader: asyncio.StreamReader,
    writer: asyncio.StreamWriter,
    *,
    auth_token: str | None = None,
) -> None:
    boot_t0 = monotonic_now()
    try:
        try:
            raw = await asyncio.wait_for(
                reader.readline(), timeout=REQUEST_READ_TIMEOUT_S
            )
        except (asyncio.LimitOverrunError, ValueError):
            # asyncio raises ``LimitOverrunError`` when no separator is found
            # within the buffer limit and plain ``ValueError`` when a
            # separator IS found but the line itself exceeds the limit. Both
            # mean "client exceeded MAX_REQUEST_BYTES" and must surface the
            # structured envelope rather than dropping the connection.
            payload = json.dumps(
                dispatcher._error_envelope(
                    "request_too_large",
                    f"daemon request exceeds {MAX_REQUEST_BYTES} byte limit",
                    {"limit": MAX_REQUEST_BYTES},
                ),
                separators=(",", ":"),
            ).encode("utf-8") + b"\n"
            writer.write(payload)
            with contextlib.suppress(Exception):
                await writer.drain()
            return
        except asyncio.TimeoutError:
            # Peer is stalled; do not write a response and let ``finally``
            # close the connection.
            return
        read_completed_at = monotonic_now()
        if not raw:
            return
        try:
            envelope = json.loads(raw.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            response = dispatcher._error_envelope(
                "bad_json",
                "daemon request must be valid JSON",
                {"message": str(exc)},
            )
        else:
            if not isinstance(envelope, dict):
                response = dispatcher._error_envelope(
                    "invalid_envelope",
                    "daemon envelope must be a JSON object",
                )
            elif auth_token is not None and envelope.pop(DAEMON_AUTH_FIELD, None) != auth_token:
                response = dispatcher._error_envelope(
                    "unauthorized",
                    "daemon request authentication failed",
                )
            else:
                response = await dispatcher.dispatch_envelope_async(
                    envelope, boot_t0=boot_t0
                )
        if isinstance(response, dict):
            timings = response.get("timings")
            if not isinstance(timings, dict):
                timings = {}
                response["timings"] = timings
            timings["runtime.read_request_s"] = max(
                0.0, read_completed_at - boot_t0
            )
        payload = json.dumps(response, separators=(",", ":")).encode("utf-8") + b"\n"
        writer.write(payload)
        await writer.drain()
    except Exception:  # pragma: no cover - logged for diagnostics
        logger.exception("daemon connection failed")
    finally:
        try:
            writer.close()
            await writer.wait_closed()
        except Exception:  # pragma: no cover
            logger.debug("daemon writer.close failed", exc_info=True)


def _prepare_socket_path(socket_path: Path) -> None:
    socket_path.parent.mkdir(parents=True, exist_ok=True)
    # Lock the parent directory to the daemon's UID before any other process
    # can race the socket bind. ``OSError`` propagates: failing to constrain
    # the parent is a deployment-fatal condition, not something to suppress.
    os.chmod(socket_path.parent, 0o700)
    if socket_path.exists() or socket_path.is_symlink():
        with contextlib.suppress(FileNotFoundError):
            socket_path.unlink()


def _write_pid(pid_path: Path) -> None:
    pid_path.parent.mkdir(parents=True, exist_ok=True)
    pid_path.write_text(f"{os.getpid()}\n", encoding="utf-8")


def _remove_pid(pid_path: Path) -> None:
    with contextlib.suppress(FileNotFoundError):
        pid_path.unlink()


async def serve(
    socket_path: Path,
    pid_path: Path,
    *,
    tcp_host: str | None = None,
    tcp_port: int | None = None,
    auth_token: str | None = None,
) -> None:
    _prepare_socket_path(socket_path)
    # Force the initial socket inode permissions to ``0o700`` via umask so the
    # window between bind() (inside ``start_unix_server``) and the explicit
    # ``os.chmod`` below is not world-accessible. The chmod that follows is
    # not allowed to fail silently: a permission-locking failure on the trust
    # boundary must surface to the daemon's exit path.
    old_umask = os.umask(0o077)
    try:
        unix_server = await asyncio.start_unix_server(
            lambda reader, writer: _handle_connection(reader, writer),
            path=str(socket_path),
            limit=MAX_REQUEST_BYTES,
        )
    finally:
        os.umask(old_umask)
    os.chmod(socket_path, 0o600)
    tcp_server: asyncio.AbstractServer | None = None
    if tcp_host and tcp_port:
        tcp_server = await asyncio.start_server(
            lambda reader, writer: _handle_connection(
                reader,
                writer,
                auth_token=auth_token,
            ),
            host=tcp_host,
            port=tcp_port,
            limit=MAX_REQUEST_BYTES,
        )
    _write_pid(pid_path)
    logger.info(
        "daemon listening on %s pid=%s tcp=%s",
        socket_path,
        os.getpid(),
        f"{tcp_host}:{tcp_port}" if tcp_server is not None else "disabled",
    )

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()

    def _signal_stop() -> None:
        stop.set()

    for sig in (signal.SIGTERM, signal.SIGINT):
        with contextlib.suppress(NotImplementedError, RuntimeError):
            loop.add_signal_handler(sig, _signal_stop)

    try:
        servers: list[asyncio.AbstractServer] = [unix_server]
        if tcp_server is not None:
            servers.append(tcp_server)
        async with contextlib.AsyncExitStack() as stack:
            for server in servers:
                await stack.enter_async_context(server)
            serve_tasks = [
                asyncio.create_task(server.serve_forever()) for server in servers
            ]
            stop_task = asyncio.create_task(stop.wait())
            done, pending = await asyncio.wait(
                {*serve_tasks, stop_task},
                return_when=asyncio.FIRST_COMPLETED,
            )
            for task in pending:
                task.cancel()
            for task in done:
                exc = task.exception()
                if exc is not None and not isinstance(exc, asyncio.CancelledError):
                    raise exc
    finally:
        from sandbox.ephemeral_workspace.pipeline_registry import stop_all_ephemeral_pipelines

        await stop_all_ephemeral_pipelines()
        _remove_pid(pid_path)
        with contextlib.suppress(FileNotFoundError):
            socket_path.unlink()


__all__ = [
    "DEFAULT_PID_PATH",
    "DEFAULT_SOCKET_PATH",
    "serve",
]
