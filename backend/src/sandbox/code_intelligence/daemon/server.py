"""Asyncio Unix-socket lifecycle for the sandbox-local CI daemon."""

from __future__ import annotations

import asyncio
import errno
import logging
import os
import signal
import time
from logging.handlers import RotatingFileHandler
from pathlib import Path
from typing import Any

from sandbox.code_intelligence.daemon import storage
from sandbox.code_intelligence.daemon.guard import (
    _dispatch_request,
    _reset_daemon_state_for_tests,
    handle_client,
)
from sandbox.code_intelligence.daemon.handlers import (
    DISPATCH,
    _deletespec_from_dict,
    _movespec_from_dict,
    _operation_change_from_dict,
    _to_dict,
    _writespec_from_dict,
    handle_ping,
    handle_shutdown,
    handle_version,
)
from sandbox.code_intelligence.daemon.state import (
    DAEMON_STATE,
    DAEMON_VERSION,
)

__all__ = [
    "DAEMON_VERSION",
    "DISPATCH",
    "DaemonAlreadyRunning",
    "_DAEMON_STATE",
    "_deletespec_from_dict",
    "_dispatch_request",
    "_movespec_from_dict",
    "_operation_change_from_dict",
    "_reset_daemon_state_for_tests",
    "_to_dict",
    "_writespec_from_dict",
    "handle_client",
    "handle_ping",
    "handle_shutdown",
    "handle_version",
    "run_daemon",
]

logger = logging.getLogger(__name__)
_DAEMON_STATE = DAEMON_STATE
_SHUTDOWN_GRACE_S = 5.0


class DaemonAlreadyRunning(Exception):
    """Raised when a live daemon already owns the state directory."""

    exit_code = 11


def _pid_is_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _read_pid(path: Path) -> int | None:
    try:
        return int(path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return None


def _prepare_state_paths(workspace_root: str) -> tuple[Path, Path, Path, Path]:
    state = storage.state_dir(workspace_root)
    socket_path = state / "daemon.sock"
    pid_path = state / "daemon.pid"
    log_path = state / "daemon.log"

    pid = _read_pid(pid_path) if pid_path.exists() else None
    if pid is not None and _pid_is_alive(pid):
        raise DaemonAlreadyRunning(f"daemon already running with pid {pid}")

    for path in (pid_path, socket_path):
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        except OSError as exc:
            if exc.errno != errno.ENOENT:
                raise
    return state, socket_path, pid_path, log_path


def _configure_file_logging(log_path: Path) -> None:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    wanted = str(log_path)
    for handler in logger.handlers:
        if isinstance(handler, RotatingFileHandler) and handler.baseFilename == wanted:
            return
    handler = RotatingFileHandler(
        log_path,
        maxBytes=10 * 1024 * 1024,
        backupCount=3,
        encoding="utf-8",
    )
    handler.setFormatter(
        logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s")
    )
    logger.addHandler(handler)
    logger.setLevel(logging.INFO)


def _build_service(state: Path, workspace_root: str) -> tuple[Any, Any, Any]:
    """Construct the daemon-resident service, ledger, and symbol index store."""
    from sandbox.code_intelligence.daemon.storage import (
        IndexStore,
        LedgerStore,
    )
    from sandbox.code_intelligence.service import CodeIntelligenceService

    ledger = LedgerStore(state_dir_path=state)
    index_store = IndexStore(state_dir_path=state)
    svc = CodeIntelligenceService(
        sandbox_id="local",
        workspace_root=workspace_root,
        sandbox=None,
        transport=None,
        edit_history=ledger,
        symbol_index_persistence=index_store,
        daemon_local=True,
    )
    return svc, ledger, index_store


def _populate_state(
    *,
    state: Path,
    workspace_root: str,
    svc: Any,
    ledger: Any,
    index_store: Any = None,
) -> None:
    """Wire process state for request dispatch."""
    DAEMON_STATE.svc = svc
    DAEMON_STATE.ledger = ledger
    DAEMON_STATE.index_store = index_store
    DAEMON_STATE.workspace_root = workspace_root
    DAEMON_STATE.started_at = time.time()
    DAEMON_STATE.guard_enabled = True
    DAEMON_STATE.guard_strict = False
    DAEMON_STATE.state_dir = state
    DAEMON_STATE.test_ops_enabled = (
        os.environ.get("EOS_CI_GUARD_TEST") == "1"
        or (state / ".allow_test_bypass_op").exists()
    )


def _kick_background_index(svc: Any) -> None:
    """Start the SymbolIndex build in the background."""
    si = getattr(svc, "symbol_index", None)
    if si is None:
        return
    try:
        si.ensure_built(wait=False)
    except Exception:  # pragma: no cover - defensive
        logger.debug("background symbol_index.ensure_built failed", exc_info=True)


async def run_daemon(workspace_root: str) -> None:
    """Start the CI daemon and return after graceful shutdown."""
    state, socket_path, pid_path, log_path = _prepare_state_paths(workspace_root)
    _configure_file_logging(log_path)

    try:
        storage.migrate_pickle_to_sqlite(state)
    except Exception:  # pragma: no cover - defensive
        logger.debug("migrate_pickle_to_sqlite failed", exc_info=True)

    svc, ledger, index_store = _build_service(state, workspace_root)
    _populate_state(
        state=state,
        workspace_root=workspace_root,
        svc=svc,
        ledger=ledger,
        index_store=index_store,
    )
    _kick_background_index(svc)

    shutdown_event = asyncio.Event()
    active_tasks: set[asyncio.Task[None]] = set()
    loop = asyncio.get_running_loop()
    installed_signals: list[signal.Signals] = []

    async def tracked_client(
        reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        task = asyncio.current_task()
        if task is not None:
            active_tasks.add(task)
        try:
            await handle_client(reader, writer)
        finally:
            if task is not None:
                active_tasks.discard(task)

    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            loop.add_signal_handler(sig, shutdown_event.set)
            installed_signals.append(sig)
        except (NotImplementedError, RuntimeError):
            logger.debug("signal handler unavailable for %s", sig, exc_info=True)

    server: asyncio.AbstractServer | None = None
    try:
        server = await asyncio.start_unix_server(tracked_client, path=str(socket_path))
        os.chmod(socket_path, 0o600)
        pid_path.write_text(f"{os.getpid()}\n", encoding="utf-8")
        logger.info("ci daemon listening on %s (state=%s)", socket_path, state)

        serve_task = asyncio.create_task(server.serve_forever())
        wait_task = asyncio.create_task(shutdown_event.wait())
        done, pending = await asyncio.wait(
            {serve_task, wait_task},
            return_when=asyncio.FIRST_COMPLETED,
        )
        if serve_task in done and serve_task.exception() is not None:
            raise serve_task.exception()  # type: ignore[misc]
        for task in pending:
            task.cancel()
    finally:
        await _shutdown_daemon(
            server=server,
            active_tasks=active_tasks,
            installed_signals=installed_signals,
            socket_path=socket_path,
            pid_path=pid_path,
        )


async def _shutdown_daemon(
    *,
    server: asyncio.AbstractServer | None,
    active_tasks: set[asyncio.Task[None]],
    installed_signals: list[signal.Signals],
    socket_path: Path,
    pid_path: Path,
) -> None:
    if server is not None:
        server.close()
        await server.wait_closed()
    if active_tasks:
        done, pending = await asyncio.wait(active_tasks, timeout=_SHUTDOWN_GRACE_S)
        del done
        for task in pending:
            task.cancel()
    loop = asyncio.get_running_loop()
    for sig in installed_signals:
        try:
            loop.remove_signal_handler(sig)
        except (NotImplementedError, RuntimeError):
            pass
    _close_daemon_services()
    for path in (socket_path, pid_path):
        try:
            path.unlink()
        except FileNotFoundError:
            pass
    logger.info("ci daemon stopped")


def _close_daemon_services() -> None:
    try:
        if DAEMON_STATE.svc is not None:
            DAEMON_STATE.svc.dispose()
    except Exception:  # pragma: no cover - defensive
        logger.debug("daemon svc.dispose failed", exc_info=True)
    if DAEMON_STATE.ledger is not None:
        try:
            DAEMON_STATE.ledger.close()
        except Exception:  # pragma: no cover - defensive
            logger.debug("ledger close failed", exc_info=True)
    if DAEMON_STATE.index_store is not None:
        try:
            DAEMON_STATE.index_store.close()
        except Exception:  # pragma: no cover - defensive
            logger.debug("index_store close failed", exc_info=True)
