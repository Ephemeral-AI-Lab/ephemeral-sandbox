"""Temporary legacy adapter for old code-intelligence daemon callers.

This module preserves the transport-backed ``DaemonCommandClient`` and the
legacy ``{ok, result, error}`` command envelope while the new runtime server
starts with an empty peer-registered op table. It is not the public runtime
server contract and is deleted in Slice 7.
"""

from __future__ import annotations

import asyncio
import base64
import dataclasses
import json
import logging
import shlex
import sys
import textwrap
import threading
import time
import traceback
from collections.abc import Sequence
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any

from sandbox.client.async_bridge import run_sync

if TYPE_CHECKING:
    from sandbox.api.models import RawExecResult
    from sandbox.occ.types import (
        EditRequest,
        EditResult,
        EditSpec,
        OperationChange,
        OperationResult,
        WriteSpec,
    )
    from sandbox.api.transport import SandboxTransport

logger = logging.getLogger(__name__)

_BUNDLE_REMOTE_DIR = "/tmp/eos-ci-runtime"
_LEGACY_COMMAND_VERSION = "0.4.0"


class DaemonCommandError(Exception):
    """Raised when the legacy daemon returns an ``ok=False`` envelope."""

    def __init__(
        self,
        kind: str,
        message: str,
        details: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(f"{kind}: {message}")
        self.kind = kind
        self.message = message
        self.details = details or {}


class DaemonCommandClient:
    """Transport-backed client for legacy code-intelligence command dispatch."""

    is_initialized: bool = False

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        *,
        transport: "SandboxTransport",
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self._transport = transport
        self.is_initialized = False
        self._init_lock = threading.Lock()

    def ensure_initialized(self, wait: bool = True) -> bool:
        del wait
        with self._init_lock:
            if self.is_initialized:
                return True
        run_sync(self._ensure_initialized_async())
        with self._init_lock:
            return self.is_initialized

    async def _ensure_initialized_async(self) -> None:
        await _ensure_runtime_uploaded_via_transport(
            self._transport,
            self.sandbox_id,
        )
        with self._init_lock:
            self.is_initialized = True

    def _call_sync(self, op: str, args: dict[str, Any] | None = None) -> Any:
        return run_sync(self._call_daemon_command(op, args or {}))

    async def _call_async(
        self,
        op: str,
        args: dict[str, Any] | None = None,
        *,
        timeout: float = 30.0,
    ) -> Any:
        return await self._call_daemon_command(op, args or {}, timeout=timeout)

    async def _call_daemon_command(
        self,
        op: str,
        args: dict[str, Any] | None = None,
        *,
        timeout: float = 30.0,
    ) -> Any:
        started = time.perf_counter()
        try:
            result = await self._call_daemon_once(
                op,
                args or {},
                timeout=timeout,
            )
            logger.debug(
                "legacy CI daemon command done: op=%s elapsed=%.3fs retry=false",
                op,
                time.perf_counter() - started,
            )
            return result
        except (ConnectionRefusedError, BrokenPipeError, FileNotFoundError, OSError):
            logger.debug("legacy CI command retry after process exec failure: op=%s", op)
            try:
                result = await self._call_daemon_once(
                    op,
                    args or {},
                    timeout=timeout,
                )
                logger.debug(
                    "legacy CI daemon command done: op=%s elapsed=%.3fs retry=true",
                    op,
                    time.perf_counter() - started,
                )
                return result
            except (
                ConnectionRefusedError,
                BrokenPipeError,
                FileNotFoundError,
                OSError,
            ) as exc:
                raise ConnectionRefusedError(
                    f"daemon unreachable after respawn: {exc}"
                ) from exc

    async def _call_daemon_once(
        self,
        op: str,
        args: dict[str, Any],
        *,
        timeout: float,
    ) -> Any:
        await _ensure_runtime_uploaded_via_transport(
            self._transport,
            self.sandbox_id,
        )
        response = await self._run_command_via_process_exec(op, args, timeout=timeout)
        logger.debug(
            "legacy CI daemon command_once: op=%s ok=%s",
            op,
            response.get("ok"),
        )
        if not response.get("ok"):
            error = response.get("error") or {}
            raise DaemonCommandError(
                kind=str(error.get("kind") or "InternalError"),
                message=str(error.get("message") or ""),
                details=error.get("details")
                if isinstance(error.get("details"), dict)
                else {},
            )
        return response.get("result")

    async def _run_command_via_process_exec(
        self, op: str, args: dict[str, Any], *, timeout: float
    ) -> dict[str, Any]:
        payload = {"op": op, "args": args}
        encoded = base64.b64encode(
            json.dumps(payload, separators=(",", ":")).encode("utf-8")
        ).decode("ascii")
        script = textwrap.dedent(
            f"""
            import base64
            import json
            import sys
            from sandbox.runtime.legacy_command_client import run_legacy_command

            payload = json.loads(base64.b64decode({encoded!r}).decode("utf-8"))
            response = run_legacy_command(
                workspace_root={self.workspace_root!r},
                op=str(payload["op"]),
                args=dict(payload.get("args") or {{}}),
            )
            raw = json.dumps(response, separators=(",", ":")).encode("utf-8")
            sys.stdout.write(base64.b64encode(raw).decode("ascii"))
            """
        ).strip()
        command = f"cd {shlex.quote(_BUNDLE_REMOTE_DIR)} && python3 - <<'PY'\n{script}\nPY"
        result = await self._transport.exec(
            self.sandbox_id,
            command,
            timeout=max(1, int(timeout) + 5),
        )
        stdout = (getattr(result, "stdout", "") or "").strip()
        if getattr(result, "exit_code", 1) != 0:
            raise ConnectionRefusedError(stdout)
        try:
            decoded = json.loads(base64.b64decode(stdout).decode("utf-8"))
        except Exception as exc:
            raise ConnectionRefusedError(
                f"daemon command produced invalid response: {stdout!r}"
            ) from exc
        if not isinstance(decoded, dict):
            raise ConnectionRefusedError(
                f"daemon command produced non-object response: {decoded!r}"
            )
        return decoded

    def warmup(self) -> None:
        self.ensure_initialized(wait=True)

    def rebind_sandbox(self, sandbox: Any) -> None:
        del sandbox
        return None

    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any:
        del sandbox
        on_progress_line = kwargs.pop("on_progress_line", None)
        timeout = kwargs.get("timeout")
        command_timeout = float(timeout if timeout is not None else 600) + 30.0
        command_started = time.perf_counter()
        raw = await self._call_async(
            "svc_cmd",
            {"command": command, **kwargs},
            timeout=command_timeout,
        )
        command_elapsed = round(time.perf_counter() - command_started, 6)
        result = SimpleNamespace(**(raw or {}))
        result.daemon_call_timings = {"total": command_elapsed}
        if on_progress_line is not None:
            progress_text = str(getattr(result, "result", "") or "")
            if progress_text:
                on_progress_line(progress_text)
        return result

    def apply(self, request: EditRequest) -> EditResult:
        from sandbox.occ.wire import (
            edit_request_to_dict,
            edit_result_from_dict,
        )

        result = self._call_sync("apply", {"request": edit_request_to_dict(request)})
        return edit_result_from_dict(result)

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        from sandbox.occ.wire import (
            operation_change_to_dict,
            operation_result_from_dict,
        )

        result = self._call_sync(
            "commit_operation_against_base",
            {
                "changes": [operation_change_to_dict(c) for c in changes],
                "agent_id": agent_id,
                "edit_type": edit_type,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]:
        from sandbox.occ.wire import operation_result_from_dict

        rows = self._call_sync("commit_specs_many", {"requests": list(requests)})
        return [operation_result_from_dict(r) for r in (rows or [])]

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        from sandbox.occ.wire import (
            normalize_write_specs,
            operation_result_from_dict,
            writespec_to_dict,
        )

        normalized = normalize_write_specs(specs)
        result = self._call_sync(
            "write_file",
            {
                "specs": [writespec_to_dict(s) for s in normalized],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        from sandbox.occ.wire import (
            editspec_to_dict,
            normalize_edit_specs,
            operation_result_from_dict,
        )

        normalized = normalize_edit_specs(specs)
        result = self._call_sync(
            "edit_file",
            {
                "specs": [editspec_to_dict(s) for s in normalized],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

    def dispose(self) -> None:
        return None


async def _ensure_runtime_uploaded_via_transport(
    transport: "SandboxTransport",
    sandbox_id: str,
) -> str:
    from sandbox.runtime.bundle import _ensure_runtime_uploaded_with_exec

    async def _exec(
        sid: str,
        command: str,
        *,
        timeout: int | None = None,
    ) -> "RawExecResult":
        return await transport.exec(sid, command, timeout=timeout)

    return await _ensure_runtime_uploaded_with_exec(sandbox_id, _exec)


def run_legacy_command(
    *, workspace_root: str, op: str, args: dict[str, Any]
) -> dict[str, Any]:
    """Run one legacy command and return the old JSON command envelope."""
    try:
        result = _dispatch_legacy(workspace_root=workspace_root, op=op, args=args)
    except KeyError:
        return {
            "ok": False,
            "error": {
                "kind": "UnsupportedOp",
                "message": f"unknown op: {op}",
                "details": {},
            },
        }
    except Exception as exc:
        return {
            "ok": False,
            "error": {
                "kind": "InternalError",
                "message": str(exc),
                "details": {"traceback": traceback.format_exc()},
            },
        }
    return {"ok": True, "result": _to_dict(result)}


def _dispatch_legacy(*, workspace_root: str, op: str, args: dict[str, Any]) -> Any:
    if op == "ping":
        return {"pong": True}
    if op == "version":
        return {"command": _LEGACY_COMMAND_VERSION, "python": sys.version}

    svc, ledger = _build_service(workspace_root)
    try:
        if op == "svc_cmd":
            return _svc_cmd(svc, args)
        if op == "apply":
            return svc.apply(_edit_request_from_dict(args["request"]))
        if op == "commit_operation_against_base":
            changes = [_operation_change_from_dict(c) for c in args.get("changes", [])]
            return svc.commit_operation_against_base(
                changes,
                agent_id=str(args.get("agent_id", "")),
                edit_type=str(args["edit_type"]),
                description=str(args.get("description", "")),
            )
        if op == "commit_specs_many":
            return svc.commit_specs_many(list(args.get("requests", [])))
        if op == "write_file":
            specs = [_writespec_from_dict(s) for s in args.get("specs", [])]
            return svc.write_file(
                specs,
                agent_id=str(args.get("agent_id", "")),
                description=str(args.get("description", "")),
            )
        if op == "edit_file":
            specs = [_editspec_from_dict(s) for s in args.get("specs", [])]
            return svc.edit_file(
                specs,
                agent_id=str(args.get("agent_id", "")),
                description=str(args.get("description", "")),
            )
        raise KeyError(op)
    finally:
        try:
            svc.dispose()
        finally:
            ledger.close()


def _build_service(workspace_root: str) -> tuple[Any, Any]:
    from sandbox.runtime.service import CodeIntelligenceService
    from sandbox.occ.state.ledger_store import LedgerStore, state_dir

    ledger = LedgerStore(state_dir_path=state_dir(workspace_root))
    svc = CodeIntelligenceService(
        sandbox_id="local",
        workspace_root=workspace_root,
        sandbox=None,
        transport=None,
        edit_history=ledger,
        daemon_local=True,
    )
    return svc, ledger


def _svc_cmd(svc: Any, args: dict[str, Any]) -> Any:
    timeout_raw = args.get("timeout")
    timeout = int(timeout_raw) if timeout_raw is not None else None
    stdin_raw = args.get("stdin")
    result = asyncio.run(
        svc.cmd(
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
    )
    return _svc_cmd_result_to_dict(result)


def _writespec_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.occ.types import WriteSpec

    return WriteSpec(
        file_path=str(d["file_path"]),
        content=str(d.get("content", "")),
        overwrite=bool(d.get("overwrite", True)),
    )


def _editspec_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.occ.types import EditSpec

    return EditSpec(
        file_path=str(d["file_path"]),
        edits=tuple(d.get("edits", ())),
    )


def _operation_change_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.occ.types import OperationChange

    return OperationChange(
        file_path=str(d["file_path"]),
        base_content=str(d.get("base_content", "")),
        base_hash=str(d.get("base_hash", "")),
        final_content=d.get("final_content"),
        base_existed=bool(d.get("base_existed", True)),
        strict_base=bool(d.get("strict_base", False)),
    )


def _edit_request_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.occ.types import EditRequest

    return EditRequest(
        file_path=str(d["file_path"]),
        old_text=str(d.get("old_text", "")),
        new_text=str(d.get("new_text", "")),
        agent_id=str(d.get("agent_id", "")),
        description=str(d.get("description", "")),
    )


def _to_dict(obj: Any) -> Any:
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
    "files_written": 0,
    "conflict_file": None,
    "conflict_reason": None,
    "warnings": [],
    "overlay_run_timings": {},
    "overlay_stage_timings": {},
}


def _svc_cmd_result_to_dict(result: Any) -> dict[str, Any]:
    return {
        field: _to_dict(getattr(result, field, default))
        for field, default in _SVC_CMD_RESULT_DEFAULTS.items()
    }


__all__ = [
    "DaemonCommandClient",
    "DaemonCommandError",
    "run_legacy_command",
]
