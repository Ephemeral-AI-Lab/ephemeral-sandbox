"""Provider-backed client for the bundled in-sandbox runtime dispatcher."""

from __future__ import annotations

import json
import logging
import shlex
import threading
import time
from types import SimpleNamespace
from typing import Any, Protocol

from sandbox.control.daemon.bundle import BUNDLE_REMOTE_DIR, ensure_runtime_uploaded
from sandbox.providers.registry import get_adapter
from sandbox.runtime.async_bridge import run_sync

logger = logging.getLogger(__name__)


class RuntimeCommandError(Exception):
    """Raised when the runtime dispatcher returns a structured error."""

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


class _RuntimeDispatchError(RuntimeError):
    """Raised when a runtime-server call fails before typed decoding."""

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


class _RuntimeExec(Protocol):
    async def __call__(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> Any: ...


async def _call_runtime_server(
    *,
    exec_fn: _RuntimeExec,
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    cwd: str = BUNDLE_REMOTE_DIR,
    timeout: int | None = None,
) -> dict[str, Any]:
    """Dispatch one JSON envelope through ``python -m sandbox.runtime.server``."""
    raw_payload = json.dumps(
        {"op": op, "args": _without_none(args)},
        separators=(",", ":"),
    )
    result = await exec_fn(
        sandbox_id,
        f"python3 -m sandbox.runtime.server {shlex.quote(raw_payload)}",
        cwd=cwd,
        timeout=timeout,
    )
    try:
        response = _decode_response(getattr(result, "stdout", ""))
    except _RuntimeDispatchError:
        if getattr(result, "exit_code", 1) != 0:
            _raise_exec_failed(result)
        raise
    if "error" in response:
        error = response.get("error") or {}
        raise _RuntimeDispatchError(
            kind=str(error.get("kind") or "RuntimeError"),
            message=str(error.get("message") or ""),
            details=error.get("details") if isinstance(error.get("details"), dict) else {},
        )
    if getattr(result, "exit_code", 1) != 0:
        _raise_exec_failed(result)
    return response


def _without_none(args: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in args.items() if value is not None}


def _decode_response(stdout: str) -> dict[str, Any]:
    try:
        decoded = json.loads((stdout or "").strip())
    except json.JSONDecodeError as exc:
        raise _RuntimeDispatchError(
            "BadRuntimeResponse",
            "runtime server returned invalid JSON",
            {"stdout": stdout},
        ) from exc
    if not isinstance(decoded, dict):
        raise _RuntimeDispatchError(
            "BadRuntimeResponse",
            "runtime server returned a non-object JSON response",
            {"response": decoded},
        )
    return decoded


def _raise_exec_failed(result: Any) -> None:
    exit_code = getattr(result, "exit_code", 1)
    raise _RuntimeDispatchError(
        kind="RuntimeExecFailed",
        message=str(getattr(result, "stderr", "") or getattr(result, "stdout", "")),
        details={"exit_code": exit_code},
    )


class RuntimeCommandClient:
    """Client for runtime-server operations executed through provider exec."""

    is_initialized: bool = False

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
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
        await ensure_runtime_uploaded(self.sandbox_id)
        with self._init_lock:
            self.is_initialized = True

    def _call_sync(self, op: str, args: dict[str, Any] | None = None) -> Any:
        return run_sync(self._call_runtime_command(op, args or {}))

    async def _call_async(
        self,
        op: str,
        args: dict[str, Any] | None = None,
        *,
        timeout: float = 30.0,
    ) -> Any:
        return await self._call_runtime_command(op, args or {}, timeout=timeout)

    async def _call_runtime_command(
        self,
        op: str,
        args: dict[str, Any] | None = None,
        *,
        timeout: float = 30.0,
    ) -> Any:
        started = time.perf_counter()
        try:
            result = await self._call_runtime_once(
                op,
                args or {},
                timeout=timeout,
            )
            logger.debug(
                "sandbox runtime command done: op=%s elapsed=%.3fs retry=false",
                op,
                time.perf_counter() - started,
            )
            return result
        except (ConnectionRefusedError, BrokenPipeError, FileNotFoundError, OSError):
            logger.debug("sandbox runtime command retry after exec failure: op=%s", op)
            try:
                result = await self._call_runtime_once(
                    op,
                    args or {},
                    timeout=timeout,
                )
                logger.debug(
                    "sandbox runtime command done: op=%s elapsed=%.3fs retry=true",
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
                    f"runtime dispatcher unreachable after retry: {exc}"
                ) from exc

    async def _call_runtime_once(
        self,
        op: str,
        args: dict[str, Any],
        *,
        timeout: float,
    ) -> Any:
        await ensure_runtime_uploaded(self.sandbox_id)
        try:
            response = await _call_runtime_server(
                exec_fn=get_adapter(self.sandbox_id).exec,
                sandbox_id=self.sandbox_id,
                op=op,
                args=args,
                timeout=max(1, int(timeout) + 5),
            )
        except _RuntimeDispatchError as exc:
            if exc.kind == "RuntimeExecFailed":
                raise ConnectionRefusedError(exc.message) from exc
            raise RuntimeCommandError(
                kind=exc.kind,
                message=exc.message,
                details=exc.details,
            ) from exc
        logger.debug(
            "sandbox runtime command_once: op=%s success=%s",
            op,
            response.get("success"),
        )
        return response

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
            "shell",
            {
                "sandbox_id": self.sandbox_id,
                "workspace_root": self.workspace_root,
                "command": command,
                **kwargs,
            },
            timeout=command_timeout,
        )
        command_elapsed = round(time.perf_counter() - command_started, 6)
        result = _shell_result_namespace(raw or {})
        result.runtime_call_timings = {"total": command_elapsed}
        if on_progress_line is not None and result.result:
            on_progress_line(result.result)
        return result

    def dispose(self) -> None:
        return None


def _shell_result_namespace(raw: dict[str, Any]) -> SimpleNamespace:
    conflict = raw.get("conflict") if isinstance(raw.get("conflict"), dict) else {}
    conflict_reason = None
    conflict_file = None
    if conflict:
        conflict_reason = conflict.get("message") or conflict.get("reason")
        conflict_file = conflict.get("conflict_file")
    changed_paths = [
        str(path) for path in (raw.get("changed_paths") or ()) if str(path or "").strip()
    ]
    return SimpleNamespace(
        result=str(raw.get("result") or ""),
        exit_code=int(raw.get("exit_code") or 0),
        changed_paths=changed_paths,
        conflict_file=conflict_file,
        conflict_reason=conflict_reason,
        warnings=list(raw.get("warnings") or ()),
        overlay_run_timings=dict(raw.get("overlay_run_timings") or {}),
        overlay_stage_timings=dict(raw.get("overlay_stage_timings") or {}),
    )


__all__ = [
    "RuntimeCommandClient",
    "RuntimeCommandError",
]
