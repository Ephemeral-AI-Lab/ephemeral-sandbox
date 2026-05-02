"""Asyncio Unix-socket daemon for sandbox-local code intelligence RPC.

Phase 2 shipped the lifecycle (spawn / ping / shutdown / kill-respawn).
Phase 3 wires the daemon to a process-resident
:class:`CodeIntelligenceService` constructed with ``sandbox=None,
transport=None`` so all local-FS branches activate, then exposes every
mutation / query / overlay verb as an RPC dispatch entry.

The same package code that the orchestrator's in-process backend runs is
reused verbatim — the daemon imports
``sandbox.code_intelligence.service.CodeIntelligenceService`` from the
Phase 1 runtime bundle. Drift surface is zero by construction.

Workspace-write bypass guard wraps every mutation handler: any file
modified within the request window that is not present in the ledger
delta is flagged ``WorkspaceBypass`` (in strict mode) or logged
(production default).
"""

from __future__ import annotations

import asyncio
import dataclasses
import errno
import logging
import os
import signal
import sys
import time
import traceback
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from logging.handlers import RotatingFileHandler
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from sandbox.code_intelligence.in_sandbox import ci_storage
from sandbox.code_intelligence.in_sandbox.ci_protocol import (
    CI_PROTOCOL_VERSION,
    FrameError,
    SchemaError,
    encode_frame,
    parse_request,
    read_frame,
)

__all__ = [
    "DAEMON_VERSION",
    "DISPATCH",
    "DaemonAlreadyRunning",
    "handle_client",
    "handle_ping",
    "handle_shutdown",
    "handle_version",
    "run_daemon",
]

logger = logging.getLogger(__name__)

DAEMON_VERSION = "0.3.0"
_STARTED_AT = time.time()
_SHUTDOWN_GRACE_S = 5.0


class DaemonAlreadyRunning(Exception):
    """Raised when a live daemon already owns the state directory."""

    exit_code = 11


# ---------------------------------------------------------------------------
# Process-level state — populated by ``run_daemon`` at startup
# ---------------------------------------------------------------------------


@dataclass
class _DaemonState:
    """Process-singleton holding the daemon's CodeIntelligenceService.

    Phase 3: ``svc`` is constructed at startup with ``sandbox=None,
    transport=None`` so local-FS branches activate. ``ledger`` is the
    SQLite-backed :class:`ci_storage.LedgerStore`. Mutation handlers route
    through ``svc``; the bypass guard wraps every write op.

    Phase 3.5: ``index_store`` is the SQLite-backed :class:`ci_storage.IndexStore`
    threaded into ``svc.symbol_index`` so per-file refreshes mirror to disk
    and the orchestrator can eventually drop its pickle-snapshot fallback.
    """

    svc: Any = None
    ledger: Any = None
    index_store: Any = None
    workspace_root: str = ""
    started_at: float = 0.0
    guard_enabled: bool = True
    guard_strict: bool = False
    state_dir: Path | None = None
    test_ops_enabled: bool = False
    extra_dispatch: dict[str, Any] = field(default_factory=dict)


_DAEMON_STATE = _DaemonState()


# Read-only ops bypass the workspace-write guard since they cannot mutate FS.
_QUERY_OPS = frozenset(
    {
        "ping",
        "version",
        "index_ready",
        "query_symbols",
        "find_definitions",
        "find_references",
        "hover",
        "diagnostics",
        "list_folder_files",
        "status",
        "get_telemetry",
    }
)


# ---------------------------------------------------------------------------
# Built-in handlers (Phase 2)
# ---------------------------------------------------------------------------


async def handle_ping(args: dict[str, Any]) -> dict[str, Any]:
    """Return daemon health."""
    del args
    return {"pong": True, "uptime_s": time.time() - _STARTED_AT}


async def handle_shutdown(args: dict[str, Any]) -> dict[str, bool]:
    """Ask the daemon process to terminate after this response drains."""
    del args
    loop = asyncio.get_running_loop()
    loop.call_later(0.05, lambda: os.kill(os.getpid(), signal.SIGTERM))
    return {"shutting_down": True}


async def handle_version(args: dict[str, Any]) -> dict[str, Any]:
    """Return protocol and runtime version details."""
    del args
    return {
        "protocol": CI_PROTOCOL_VERSION,
        "daemon": DAEMON_VERSION,
        "python": sys.version,
    }


async def handle_set_guard_mode(args: dict[str, Any]) -> dict[str, Any]:
    """Toggle strict/enabled flags on the bypass guard (test/diagnostic only)."""
    if not _DAEMON_STATE.test_ops_enabled:
        raise RuntimeError("_set_guard_mode is disabled in this daemon")
    if "strict" in args:
        _DAEMON_STATE.guard_strict = bool(args["strict"])
    if "enabled" in args:
        _DAEMON_STATE.guard_enabled = bool(args["enabled"])
    return {
        "guard_enabled": _DAEMON_STATE.guard_enabled,
        "guard_strict": _DAEMON_STATE.guard_strict,
    }


async def handle_index_ready(args: dict[str, Any]) -> dict[str, Any]:
    del args
    svc = _DAEMON_STATE.svc
    if svc is None:
        return {"ready": False}
    si = getattr(svc, "symbol_index", None)
    return {"ready": bool(getattr(si, "is_built", False))}


# ---------------------------------------------------------------------------
# Phase 3 — code-intelligence dispatch handlers
# ---------------------------------------------------------------------------


def _require_svc() -> Any:
    svc = _DAEMON_STATE.svc
    if svc is None:
        raise RuntimeError("daemon CodeIntelligenceService not initialized")
    return svc


def _writespec_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.code_intelligence.core.types import WriteSpec

    return WriteSpec(
        file_path=str(d["file_path"]),
        content=str(d.get("content", "")),
        overwrite=bool(d.get("overwrite", True)),
    )


def _editspec_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.code_intelligence.core.types import EditSpec

    return EditSpec(
        file_path=str(d["file_path"]),
        edits=tuple(d.get("edits", ())),
    )


def _movespec_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.code_intelligence.core.types import MoveSpec

    return MoveSpec(
        src_path=str(d.get("src_path") or d.get("source") or ""),
        dst_path=str(d.get("dst_path") or d.get("destination") or ""),
        overwrite=bool(d.get("overwrite", False)),
        is_folder=bool(d.get("is_folder", False)),
    )


def _deletespec_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.code_intelligence.core.types import DeleteSpec

    return DeleteSpec(
        path=str(d.get("path") or d.get("file_path") or ""),
        is_folder=bool(d.get("is_folder", False)),
    )


def _operation_change_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.code_intelligence.core.types import OperationChange

    return OperationChange(
        file_path=str(d["file_path"]),
        base_content=str(d.get("base_content", "")),
        base_hash=str(d.get("base_hash", "")),
        final_content=d.get("final_content"),
        base_existed=bool(d.get("base_existed", True)),
        strict_base=bool(d.get("strict_base", False)),
    )


def _edit_request_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.code_intelligence.core.types import EditRequest

    return EditRequest(
        file_path=str(d["file_path"]),
        old_text=str(d.get("old_text", "")),
        new_text=str(d.get("new_text", "")),
        agent_id=str(d.get("agent_id", "")),
        description=str(d.get("description", "")),
    )


def _to_dict(obj: Any) -> Any:
    """Convert dataclasses (recursively) into JSON/msgpack-safe dicts."""
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        return {k: _to_dict(v) for k, v in dataclasses.asdict(obj).items()}
    if isinstance(obj, SimpleNamespace):
        return {str(k): _to_dict(v) for k, v in vars(obj).items()}
    if isinstance(obj, (list, tuple)):
        return [_to_dict(v) for v in obj]
    if isinstance(obj, dict):
        return {str(k): _to_dict(v) for k, v in obj.items()}
    return obj


_SVC_CMD_RESULT_DEFAULTS: dict[str, Any] = {
    "result": "",
    "exit_code": 1,
    "changed_paths": [],
    "ambient_changed_paths": [],
    "files_written": 0,
    "git_commit_status": None,
    "git_conflict_file": None,
    "git_conflict_reason": None,
    "gitinclude_changed_paths": [],
    "gitignore_direct_merged_paths": [],
    "gitignore_direct_merged_count": 0,
    "mixed_gitinclude_gitignore": False,
    "mixed_partial_apply": False,
    "warnings": [],
    "git_snapshot_timings": {},
    "overlay_run_timings": {},
}


def _svc_cmd_result_to_dict(result: Any) -> dict[str, Any]:
    """Preserve the audited shell ``SimpleNamespace`` contract over msgpack."""
    return {
        field: _to_dict(getattr(result, field, default))
        for field, default in _SVC_CMD_RESULT_DEFAULTS.items()
    }


async def handle_query_symbols(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    return [_to_dict(s) for s in svc.query_symbols(str(args.get("query", "")))]


async def handle_find_definitions(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    return [
        _to_dict(s)
        for s in svc.find_definitions(
            str(args["file_path"]),
            str(args.get("symbol", "")),
            int(args.get("line", 0)),
            int(args.get("character", 0)),
        )
    ]


async def handle_find_references(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    return [
        _to_dict(s)
        for s in svc.find_references(
            str(args["file_path"]),
            str(args.get("symbol", "")),
            int(args.get("line", 0)),
            int(args.get("character", 0)),
        )
    ]


async def handle_hover(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    result = svc.hover(
        str(args["file_path"]),
        int(args["line"]),
        int(args.get("character", 0)),
    )
    return _to_dict(result) if result is not None else None


async def handle_diagnostics(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    return [_to_dict(d) for d in svc.diagnostics(str(args["file_path"]))]


async def handle_list_folder_files(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    return list(svc.list_folder_files(str(args["folder"])))


async def handle_status(args: dict[str, Any]) -> Any:
    del args
    svc = _require_svc()
    return _to_dict(svc.status())


async def handle_get_telemetry(args: dict[str, Any]) -> Any:
    del args
    svc = _require_svc()
    return _to_dict(svc.get_telemetry())


async def handle_svc_cmd(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    timeout_raw = args.get("timeout")
    timeout = int(timeout_raw) if timeout_raw is not None else None
    stdin_raw = args.get("stdin")
    result = await svc.cmd(
        None,
        str(args["command"]),
        timeout=timeout,
        description=str(args.get("description", "")),
        agent_id=str(args.get("agent_id", "")),
        run_id=str(args.get("run_id", "")),
        agent_run_id=str(args.get("agent_run_id", "")),
        task_id=str(args.get("task_id", "")),
        stdin=str(stdin_raw) if stdin_raw is not None else None,
        attribute_changes=bool(args.get("attribute_changes", True)),
    )
    return _svc_cmd_result_to_dict(result)


async def handle_apply_edit(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    request = _edit_request_from_dict(args["request"])
    return _to_dict(svc.apply_edit(request))


async def handle_commit_operation_against_base(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    changes = [_operation_change_from_dict(c) for c in args.get("changes", [])]
    return _to_dict(
        svc.commit_operation_against_base(
            changes,
            agent_id=str(args.get("agent_id", "")),
            edit_type=str(args["edit_type"]),
            description=str(args.get("description", "")),
        )
    )


async def handle_commit_specs_many(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    requests = list(args.get("requests", []))
    return [_to_dict(r) for r in svc.commit_specs_many(requests)]


async def handle_write_file(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    specs = [_writespec_from_dict(s) for s in args.get("specs", [])]
    return _to_dict(
        svc.write_file(
            specs,
            agent_id=str(args.get("agent_id", "")),
            description=str(args.get("description", "")),
        )
    )


async def handle_edit_file(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    specs = [_editspec_from_dict(s) for s in args.get("specs", [])]
    return _to_dict(
        svc.edit_file(
            specs,
            agent_id=str(args.get("agent_id", "")),
            description=str(args.get("description", "")),
        )
    )


async def handle_delete_file(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    raw_paths = args.get("paths", [])
    paths: list[Any] = []
    for entry in raw_paths:
        if isinstance(entry, dict):
            paths.append(_deletespec_from_dict(entry))
        else:
            paths.append(str(entry))
    return _to_dict(
        svc.delete_file(
            paths,
            agent_id=str(args.get("agent_id", "")),
            description=str(args.get("description", "")),
        )
    )


async def handle_move_file(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    specs = [_movespec_from_dict(s) for s in args.get("specs", [])]
    return _to_dict(
        svc.move_file(
            specs,
            agent_id=str(args.get("agent_id", "")),
            description=str(args.get("description", "")),
        )
    )


async def handle_undo_last_edit(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    return _to_dict(svc.undo_last_edit(str(args["file_path"])))


async def handle_index_refresh(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    si = getattr(svc, "symbol_index", None)
    if si is None:
        return {"generation": 0}
    gen = si.refresh(str(args["file_path"]), args.get("content"))
    return {"generation": int(gen)}


async def handle_lsp_invalidate(args: dict[str, Any]) -> Any:
    svc = _require_svc()
    lsp = getattr(svc, "lsp_client", None)
    file_path = args.get("file_path")
    if lsp is None or not hasattr(lsp, "invalidate_file"):
        return {"invalidated": False}
    if file_path is None:
        # Best-effort: most LspClient implementations expose invalidate_file
        # only on a per-path level. Surface a clear payload.
        return {"invalidated": False, "reason": "no file_path supplied"}
    try:
        lsp.invalidate_file(str(file_path))
    except Exception as exc:  # pragma: no cover - defensive
        logger.warning("lsp_invalidate failed for %s: %s", file_path, exc)
        return {"invalidated": False, "error": str(exc)}
    return {"invalidated": True, "file_path": str(file_path)}


# ---------------------------------------------------------------------------
# Dispatch table
# ---------------------------------------------------------------------------


DISPATCH: dict[str, Callable[[dict[str, Any]], Awaitable[Any]]] = {
    # Lifecycle (Phase 2)
    "ping": handle_ping,
    "shutdown": handle_shutdown,
    "version": handle_version,
    # Phase 3 — queries
    "query_symbols": handle_query_symbols,
    "find_definitions": handle_find_definitions,
    "find_references": handle_find_references,
    "hover": handle_hover,
    "diagnostics": handle_diagnostics,
    "list_folder_files": handle_list_folder_files,
    "status": handle_status,
    "get_telemetry": handle_get_telemetry,
    "svc_cmd": handle_svc_cmd,
    # Phase 3 — mutations
    "apply_edit": handle_apply_edit,
    "commit_operation_against_base": handle_commit_operation_against_base,
    "commit_specs_many": handle_commit_specs_many,
    "write_file": handle_write_file,
    "edit_file": handle_edit_file,
    "delete_file": handle_delete_file,
    "move_file": handle_move_file,
    "undo_last_edit": handle_undo_last_edit,
    # Phase 3 — internal
    "index_refresh": handle_index_refresh,
    "lsp_invalidate": handle_lsp_invalidate,
    "index_ready": handle_index_ready,
    "_set_guard_mode": handle_set_guard_mode,
}


# ---------------------------------------------------------------------------
# Workspace-write bypass guard
# ---------------------------------------------------------------------------


def _ledger_total_edits() -> int:
    svc = _DAEMON_STATE.svc
    if svc is None:
        return 0
    arbiter = getattr(svc, "arbiter", None)
    metrics = getattr(arbiter, "metrics", None) if arbiter is not None else None
    return int(getattr(metrics, "total_edits", 0))


def _ledger_paths_since(window_start: float) -> set[str]:
    """Return file paths recorded in the ledger after ``window_start``."""
    ledger = _DAEMON_STATE.ledger
    if ledger is None:
        return set()
    try:
        records = ledger.changes_since(window_start)
    except Exception:  # pragma: no cover - defensive
        logger.debug("ledger.changes_since failed for window=%s", window_start, exc_info=True)
        return set()
    return {r.file_path for r in records}


_GUARD_SCAN_WINDOW = 1.0  # seconds tolerance on either side
# Skip directories that habitually carry mtime churn unrelated to user
# mutations (build outputs, vendored deps, IDE caches). The guard remains
# O(remaining_files) per mutation; Phase 3.5's perf E2E will surface any
# remaining hot spots and may move the guard onto an inotify/fanotify-style
# watch instead of a per-request walk.
_GUARD_IGNORE_DIRS = {
    ".git",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".venv",
    "venv",
    "node_modules",
    "build",
    "dist",
    ".cache",
    ".idea",
    ".vscode",
    "target",  # rust/cargo
    ".gradle",
}


def _scan_unledgered_changes(
    workspace_root: str, window_start: float, ledger_paths: set[str]
) -> list[str]:
    """Return file paths under ``workspace_root`` modified during the window
    that do not appear in ``ledger_paths`` (exact-string match).

    Detection only — does not block the write.
    """
    root = Path(workspace_root)
    if not root.exists():
        return []
    cutoff = window_start
    bypassed: list[str] = []
    try:
        for current_dir, dirnames, filenames in os.walk(root):
            dirnames[:] = [d for d in dirnames if d not in _GUARD_IGNORE_DIRS]
            for name in filenames:
                full = os.path.join(current_dir, name)
                try:
                    mtime = os.path.getmtime(full)
                except OSError:
                    continue
                if mtime <= cutoff:
                    continue
                if full in ledger_paths:
                    continue
                bypassed.append(full)
    except OSError:  # pragma: no cover - defensive
        logger.debug("guard scan failed under %s", root, exc_info=True)
    return bypassed


# ---------------------------------------------------------------------------
# Request lifecycle
# ---------------------------------------------------------------------------


async def _dispatch_request(body: dict[str, Any]) -> dict[str, Any]:
    """Run one validated RPC request and return a response envelope."""
    try:
        request = parse_request(body)
    except SchemaError as exc:
        return {
            "v": CI_PROTOCOL_VERSION,
            "id": str(body.get("id") or ""),
            "ok": False,
            "error": {"kind": "InvalidSchema", "message": str(exc), "details": {}},
        }

    handler = DISPATCH.get(request.op) or _DAEMON_STATE.extra_dispatch.get(request.op)
    if handler is None:
        return {
            "v": CI_PROTOCOL_VERSION,
            "id": request.id,
            "ok": False,
            "error": {
                "kind": "UnsupportedOp",
                "message": f"unknown op: {request.op}",
                "details": {},
            },
        }

    is_query = request.op in _QUERY_OPS
    window_start = time.time() - _GUARD_SCAN_WINDOW
    pre_seq = 0 if is_query else _ledger_total_edits()

    try:
        result = await handler(request.args)
    except Exception as exc:  # pragma: no cover - defensive envelope path
        logger.exception("ci daemon handler failed for op=%s", request.op)
        return {
            "v": CI_PROTOCOL_VERSION,
            "id": request.id,
            "ok": False,
            "error": {
                "kind": "InternalError",
                "message": str(exc),
                "details": {"traceback": traceback.format_exc()},
            },
        }

    success_envelope = {
        "v": CI_PROTOCOL_VERSION,
        "id": request.id,
        "ok": True,
        "result": result,
    }

    # Bypass guard runs only on mutation ops.
    if (
        not is_query
        and _DAEMON_STATE.guard_enabled
        and _DAEMON_STATE.svc is not None
        and _DAEMON_STATE.workspace_root
    ):
        post_seq = _ledger_total_edits()
        if post_seq > pre_seq:
            ledger_paths = _ledger_paths_since(window_start)
        else:
            ledger_paths = set()
        bypassed = _scan_unledgered_changes(
            _DAEMON_STATE.workspace_root, window_start, ledger_paths
        )
        if bypassed:
            logger.error(
                "WORKSPACE WRITE BYPASS: handler=%s bypassed paths=%s",
                request.op,
                bypassed,
            )
            if _DAEMON_STATE.guard_strict:
                return {
                    "v": CI_PROTOCOL_VERSION,
                    "id": request.id,
                    "ok": False,
                    "error": {
                        "kind": "WorkspaceBypass",
                        "message": (
                            f"unledgered writes during op={request.op}: {bypassed}"
                        ),
                        "details": {"paths": bypassed},
                    },
                }
    return success_envelope


async def handle_client(
    reader: asyncio.StreamReader, writer: asyncio.StreamWriter
) -> None:
    """Serve requests on one Unix-socket connection."""
    peer = writer.get_extra_info("peername")
    try:
        while not reader.at_eof():
            try:
                body = await read_frame(reader)
            except (FrameError, SchemaError, asyncio.IncompleteReadError):
                logger.debug("closing malformed ci daemon connection from %r", peer)
                break
            response = await _dispatch_request(body)
            writer.write(encode_frame(response))
            await writer.drain()
    finally:
        writer.close()
        await writer.wait_closed()


# ---------------------------------------------------------------------------
# Lifecycle
# ---------------------------------------------------------------------------


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
    state = ci_storage.state_dir(workspace_root)
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
    """Construct the daemon-resident :class:`CodeIntelligenceService` + ledger
    + index store.

    Imports are deferred so that the storage layer (``ci_storage``) remains
    importable in environments where the heavyweight CI dependencies are
    optional.

    Phase 3.5: an :class:`IndexStore` is constructed and threaded into
    ``CodeIntelligenceService(symbol_index_persistence=...)`` so per-file
    refreshes mirror to ``index.sqlite3`` instead of rewriting the pickle.
    """
    from sandbox.code_intelligence.in_sandbox.ci_storage import (
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
    """Wire ``_DAEMON_STATE`` for the request lifecycle."""
    _DAEMON_STATE.svc = svc
    _DAEMON_STATE.ledger = ledger
    _DAEMON_STATE.index_store = index_store
    _DAEMON_STATE.workspace_root = workspace_root
    _DAEMON_STATE.started_at = time.time()
    _DAEMON_STATE.guard_enabled = True
    _DAEMON_STATE.guard_strict = False
    _DAEMON_STATE.state_dir = state
    _DAEMON_STATE.test_ops_enabled = (
        os.environ.get("EOS_CI_GUARD_TEST") == "1"
        or (state / ".allow_test_bypass_op").exists()
    )


def _kick_background_index(svc: Any) -> None:
    """SOCKET-FIRST: start the SymbolIndex build in the background."""
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

    # Phase 3.5: drain any pre-existing pickle snapshot into the SQLite index
    # before the IndexStore is opened so the new daemon serves queries from a
    # warm-cache state on first run after migration.
    try:
        ci_storage.migrate_pickle_to_sqlite(state)
    except Exception:  # pragma: no cover - defensive
        logger.debug("migrate_pickle_to_sqlite failed", exc_info=True)

    # Phase 3: daemon-resident CodeIntelligenceService + SQLite ledger
    # Phase 3.5: + SQLite IndexStore for the symbol index
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
        server = await asyncio.start_unix_server(
            tracked_client,
            path=str(socket_path),
        )
        os.chmod(socket_path, 0o600)
        # Write PID AFTER socket bind so launcher's readiness poll succeeds the
        # moment the socket is reachable.
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
        if server is not None:
            server.close()
            await server.wait_closed()
        if active_tasks:
            done, pending = await asyncio.wait(active_tasks, timeout=_SHUTDOWN_GRACE_S)
            del done
            for task in pending:
                task.cancel()
        for sig in installed_signals:
            try:
                loop.remove_signal_handler(sig)
            except (NotImplementedError, RuntimeError):
                pass
        # Phase 3.6: tear down the persistent LSP child BEFORE svc.dispose so
        # the basedpyright langserver gets the LSP shutdown handshake while
        # the host loop is still alive. ``svc.dispose`` calls
        # ``LspClient.close`` which collapses the LspAsyncHost — at that point
        # the lsp_child's loop is gone and graceful shutdown becomes a no-op.
        try:
            if _DAEMON_STATE.svc is not None:
                _DAEMON_STATE.svc.dispose()
        except Exception:  # pragma: no cover - defensive
            logger.debug("daemon svc.dispose failed", exc_info=True)
        if _DAEMON_STATE.ledger is not None:
            try:
                _DAEMON_STATE.ledger.close()
            except Exception:  # pragma: no cover - defensive
                logger.debug("ledger close failed", exc_info=True)
        if _DAEMON_STATE.index_store is not None:
            try:
                _DAEMON_STATE.index_store.close()
            except Exception:  # pragma: no cover - defensive
                logger.debug("index_store close failed", exc_info=True)
        for path in (socket_path, pid_path):
            try:
                path.unlink()
            except FileNotFoundError:
                pass
        logger.info("ci daemon stopped")


# ---------------------------------------------------------------------------
# Internal helpers used by tests
# ---------------------------------------------------------------------------


def _reset_daemon_state_for_tests(extra_dispatch: dict[str, Any] | None = None) -> None:
    """Reset module-level state between unit tests. Not a public API."""
    _DAEMON_STATE.svc = None
    _DAEMON_STATE.ledger = None
    _DAEMON_STATE.index_store = None
    _DAEMON_STATE.workspace_root = ""
    _DAEMON_STATE.started_at = 0.0
    _DAEMON_STATE.guard_enabled = True
    _DAEMON_STATE.guard_strict = False
    _DAEMON_STATE.state_dir = None
    _DAEMON_STATE.test_ops_enabled = False
    _DAEMON_STATE.extra_dispatch = dict(extra_dispatch or {})
