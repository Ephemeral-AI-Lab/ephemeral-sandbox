"""Daemon command handlers for sandbox-local code intelligence."""

from __future__ import annotations

import asyncio
import dataclasses
import logging
import os
import signal
import sys
import time
from collections.abc import Awaitable, Callable
from types import SimpleNamespace
from typing import Any

from sandbox.code_intelligence.daemon.protocol import CI_PROTOCOL_VERSION
from sandbox.code_intelligence.daemon.state import (
    DAEMON_STATE,
    DAEMON_VERSION,
    STARTED_AT,
)

logger = logging.getLogger(__name__)


async def handle_ping(args: dict[str, Any]) -> dict[str, Any]:
    """Return daemon health."""
    del args
    return {"pong": True, "uptime_s": time.time() - STARTED_AT}


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
    """Toggle strict/enabled flags on the bypass guard."""
    if not _test_ops_enabled():
        raise RuntimeError("_set_guard_mode is disabled in this daemon")
    if "strict" in args:
        DAEMON_STATE.guard_strict = bool(args["strict"])
    if "enabled" in args:
        DAEMON_STATE.guard_enabled = bool(args["enabled"])
    return {
        "guard_enabled": DAEMON_STATE.guard_enabled,
        "guard_strict": DAEMON_STATE.guard_strict,
    }


def _test_ops_enabled() -> bool:
    """Return whether test-only daemon ops are enabled for this process."""
    if DAEMON_STATE.test_ops_enabled or os.environ.get("EOS_CI_GUARD_TEST") == "1":
        return True
    state = DAEMON_STATE.state_dir
    return bool(state is not None and (state / ".allow_test_bypass_op").exists())


async def handle_index_ready(args: dict[str, Any]) -> dict[str, Any]:
    del args
    svc = DAEMON_STATE.svc
    if svc is None:
        return {"ready": False}
    si = getattr(svc, "symbol_index", None)
    return {"ready": bool(getattr(si, "is_built", False))}


def _require_svc() -> Any:
    svc = DAEMON_STATE.svc
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
    """Convert dataclasses recursively into msgpack-safe objects."""
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
    "overlay_run_timings": {},
    "overlay_stage_timings": {},
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
    return _to_dict(svc.apply_edit(_edit_request_from_dict(args["request"])))


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
    return [_to_dict(r) for r in svc.commit_specs_many(list(args.get("requests", [])))]


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
    paths: list[Any] = []
    for entry in args.get("paths", []):
        paths.append(_deletespec_from_dict(entry) if isinstance(entry, dict) else str(entry))
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
        return {"invalidated": False, "reason": "no file_path supplied"}
    try:
        lsp.invalidate_file(str(file_path))
    except Exception as exc:  # pragma: no cover - defensive
        logger.warning("lsp_invalidate failed for %s: %s", file_path, exc)
        return {"invalidated": False, "error": str(exc)}
    return {"invalidated": True, "file_path": str(file_path)}


DISPATCH: dict[str, Callable[[dict[str, Any]], Awaitable[Any]]] = {
    "ping": handle_ping,
    "shutdown": handle_shutdown,
    "version": handle_version,
    "query_symbols": handle_query_symbols,
    "find_definitions": handle_find_definitions,
    "find_references": handle_find_references,
    "hover": handle_hover,
    "diagnostics": handle_diagnostics,
    "list_folder_files": handle_list_folder_files,
    "status": handle_status,
    "get_telemetry": handle_get_telemetry,
    "svc_cmd": handle_svc_cmd,
    "apply_edit": handle_apply_edit,
    "commit_operation_against_base": handle_commit_operation_against_base,
    "commit_specs_many": handle_commit_specs_many,
    "write_file": handle_write_file,
    "edit_file": handle_edit_file,
    "delete_file": handle_delete_file,
    "move_file": handle_move_file,
    "undo_last_edit": handle_undo_last_edit,
    "index_refresh": handle_index_refresh,
    "lsp_invalidate": handle_lsp_invalidate,
    "index_ready": handle_index_ready,
    "_set_guard_mode": handle_set_guard_mode,
}
