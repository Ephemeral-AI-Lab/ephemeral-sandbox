"""Client for the sandbox-local code-intelligence daemon."""

from __future__ import annotations

import asyncio
import base64
import logging
import textwrap
import threading
import time
import uuid
from collections.abc import Sequence
from types import SimpleNamespace
from typing import Any

from sandbox.api.transport import SandboxTransport
from sandbox.client.async_bridge import run_sync
from sandbox.code_intelligence.daemon.wire import (
    deletespec_to_dict,
    edit_request_to_dict,
    edit_result_from_dict,
    editspec_to_dict,
    movespec_to_dict,
    normalize_edit_specs,
    normalize_write_specs,
    operation_change_to_dict,
    operation_result_from_dict,
    symbol_info_from_dict,
    telemetry_from_dict,
    writespec_to_dict,
)
from sandbox.code_intelligence.core.types import (
    CITelemetry,
    DeleteSpec,
    EditRequest,
    EditResult,
    EditSpec,
    MoveSpec,
    OperationChange,
    OperationResult,
    SymbolInfo,
    WriteSpec,
)
from sandbox.code_intelligence.daemon.protocol import (
    CI_PROTOCOL_VERSION,
    encode_frame,
    parse_response,
    read_frame,
)

logger = logging.getLogger(__name__)


class DaemonCommandError(Exception):
    """Raised when the daemon returns an ``ok=False`` command envelope."""

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
    """Transport-backed client for daemon command dispatch.

    The daemon owns the canonical SQLite ``IndexStore`` and serves every
    code-intelligence verb through framed msgpack command dispatch.
    """

    is_initialized: bool = False
    _INDEX_READY_TIMEOUT_S: float = 60.0
    _INDEX_READY_POLL_S: float = 0.5

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        *,
        transport: SandboxTransport,
    ) -> None:
        from sandbox.code_intelligence.daemon.launcher import DaemonLauncher

        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self._transport = transport
        self._launcher = DaemonLauncher(transport, sandbox_id, workspace_root)
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
        """Launch the daemon and wait for the background SymbolIndex build."""
        await self._launcher.ensure_daemon()

        deadline = asyncio.get_event_loop().time() + self._INDEX_READY_TIMEOUT_S
        while True:
            try:
                resp = await self._call_daemon_command("index_ready", {})
            except Exception as exc:  # pragma: no cover - exposed via tests
                logger.debug(
                    "index_ready call failed during ensure_initialized: %s", exc
                )
                resp = None
            if isinstance(resp, dict) and resp.get("ready"):
                break
            if asyncio.get_event_loop().time() >= deadline:
                break
            await asyncio.sleep(self._INDEX_READY_POLL_S)
        with self._init_lock:
            self.is_initialized = True

    def _call_sync(self, op: str, args: dict[str, Any] | None = None) -> Any:
        """Send one daemon command synchronously."""
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
        """Send one framed command to the in-sandbox daemon."""
        from sandbox.code_intelligence.daemon.launcher import DaemonUnavailable

        started = time.perf_counter()
        try:
            result = await self._call_daemon_once(
                self._launcher,
                op,
                args or {},
                timeout=timeout,
            )
            logger.debug(
                "ci daemon command done: op=%s elapsed=%.3fs retry=false",
                op,
                time.perf_counter() - started,
            )
            return result
        except (ConnectionRefusedError, BrokenPipeError, FileNotFoundError, OSError):
            retry_started = time.perf_counter()
            await self._launcher.ensure_daemon()
            logger.debug(
                "ci daemon command retry after ensure_daemon: "
                "op=%s ensure_elapsed=%.3fs",
                op,
                time.perf_counter() - retry_started,
            )
            try:
                result = await self._call_daemon_once(
                    self._launcher,
                    op,
                    args or {},
                    timeout=timeout,
                )
                logger.debug(
                    "ci daemon command done: op=%s elapsed=%.3fs retry=true",
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
                raise DaemonUnavailable(
                    f"daemon unreachable after respawn: {exc}"
                ) from exc

    async def _call_daemon_once(
        self,
        launcher: Any,
        op: str,
        args: dict[str, Any],
        *,
        timeout: float,
    ) -> Any:
        request_id = uuid.uuid4().hex
        frame = encode_frame(
            {"v": CI_PROTOCOL_VERSION, "id": request_id, "op": op, "args": args}
        )
        socket_started = time.perf_counter()
        socket_path = await launcher.socket_path()
        socket_elapsed = time.perf_counter() - socket_started
        send_started = time.perf_counter()
        response_frame = await self._send_frame_via_process_exec(
            socket_path,
            frame,
            timeout=timeout,
        )
        send_elapsed = time.perf_counter() - send_started
        parse_started = time.perf_counter()
        reader = asyncio.StreamReader()
        reader.feed_data(response_frame)
        reader.feed_eof()
        response = parse_response(await read_frame(reader))
        parse_elapsed = time.perf_counter() - parse_started
        logger.debug(
            "ci daemon command_once: op=%s request_id=%s socket_path_elapsed=%.3fs "
            "send_frame_elapsed=%.3fs parse_elapsed=%.3fs "
            "request_bytes=%d response_bytes=%d",
            op,
            request_id,
            socket_elapsed,
            send_elapsed,
            parse_elapsed,
            len(frame),
            len(response_frame),
        )
        if response.id != request_id:
            raise RuntimeError(
                f"daemon response id mismatch: expected {request_id}, got {response.id}"
            )
        if not response.ok:
            error = response.error or {}
            raise DaemonCommandError(
                kind=str(error.get("kind") or "InternalError"),
                message=str(error.get("message") or ""),
                details=error.get("details")
                if isinstance(error.get("details"), dict)
                else {},
            )
        return response.result

    async def _send_frame_via_process_exec(
        self,
        socket_path: str,
        frame: bytes,
        *,
        timeout: float,
    ) -> bytes:
        """Send ``frame`` through a sandbox-local Python Unix-socket bridge."""
        encoded = base64.b64encode(frame).decode("ascii")
        script = textwrap.dedent(
            f"""
            import base64
            import socket
            import sys

            frame = base64.b64decode({encoded!r})
            sock = socket.socket(socket.AF_UNIX)
            sock.settimeout({float(timeout)!r})
            sock.connect({socket_path!r})
            sock.sendall(frame)
            sock.shutdown(socket.SHUT_WR)
            chunks = []
            while True:
                data = sock.recv(65536)
                if not data:
                    break
                chunks.append(data)
            sock.close()
            sys.stdout.write(base64.b64encode(b"".join(chunks)).decode("ascii"))
            """
        ).strip()
        command = f"python3 - <<'PY'\n{script}\nPY"
        result = await self._transport.exec(
            self.sandbox_id,
            command,
            timeout=max(1, int(timeout) + 5),
        )
        stdout = (getattr(result, "stdout", "") or "").strip()
        if getattr(result, "exit_code", 1) != 0:
            raise ConnectionRefusedError(stdout)
        try:
            return base64.b64decode(stdout)
        except Exception as exc:
            raise ConnectionRefusedError(
                f"daemon bridge produced invalid base64: {stdout!r}"
            ) from exc

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

    def query_symbols(self, query: str) -> list[SymbolInfo]:
        rows = self._call_sync("query_symbols", {"query": query})
        return [symbol_info_from_dict(r) for r in (rows or [])]

    def apply_edit(self, request: EditRequest) -> EditResult:
        result = self._call_sync("apply_edit", {"request": edit_request_to_dict(request)})
        return edit_result_from_dict(result)

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
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
        rows = self._call_sync("commit_specs_many", {"requests": list(requests)})
        return [operation_result_from_dict(r) for r in (rows or [])]

    def list_folder_files(self, folder: str) -> list[str]:
        rows = self._call_sync("list_folder_files", {"folder": folder})
        return list(rows or [])

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
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

    def delete_file(
        self,
        paths: Sequence[str | DeleteSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        encoded: list[Any] = []
        for entry in paths:
            encoded.append(entry if isinstance(entry, str) else deletespec_to_dict(entry))
        result = self._call_sync(
            "delete_file",
            {"paths": encoded, "agent_id": agent_id, "description": description},
        )
        return operation_result_from_dict(result)

    def move_file(
        self,
        specs: Sequence[MoveSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        result = self._call_sync(
            "move_file",
            {
                "specs": [movespec_to_dict(s) for s in specs],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

    def undo_last_edit(self, file_path: str) -> EditResult:
        result = self._call_sync("undo_last_edit", {"file_path": file_path})
        return edit_result_from_dict(result)

    def status(self) -> dict[str, Any]:
        result = self._call_sync("status")
        return dict(result or {})

    def get_telemetry(self) -> CITelemetry:
        result = self._call_sync("get_telemetry")
        return telemetry_from_dict(result or {})

    def dispose(self) -> None:
        try:
            run_sync(self._launcher.shutdown())
        except Exception:
            logger.debug(
                "CI daemon shutdown skipped for sandbox %s",
                self.sandbox_id,
                exc_info=True,
            )
