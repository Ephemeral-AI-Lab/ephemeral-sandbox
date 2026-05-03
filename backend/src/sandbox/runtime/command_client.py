"""Provider-backed client for the bundled in-sandbox runtime dispatcher."""

from __future__ import annotations

import logging
import threading
import time
from collections.abc import Sequence
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any

from sandbox.client.async_bridge import run_sync
from sandbox.providers.registry import get_adapter
from sandbox.runtime.bundle import ensure_runtime_uploaded
from sandbox.runtime._server_dispatch import RuntimeDispatchError, call_runtime_server

if TYPE_CHECKING:
    from sandbox.occ.types import (
        EditRequest,
        EditResult,
        EditSpec,
        OperationChange,
        OperationResult,
        WriteSpec,
    )

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
            response = await call_runtime_server(
                exec_fn=get_adapter(self.sandbox_id).exec,
                sandbox_id=self.sandbox_id,
                op=op,
                args=args,
                timeout=max(1, int(timeout) + 5),
            )
        except RuntimeDispatchError as exc:
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

    def apply(self, request: "EditRequest") -> "EditResult":
        from sandbox.occ.wire import (
            edit_request_to_dict,
            edit_result_from_dict,
        )

        result = self._call_sync(
            "occ.apply",
            {
                "workspace_root": self.workspace_root,
                "request": edit_request_to_dict(request),
            },
        )
        return edit_result_from_dict(result)

    def commit_operation_against_base(
        self,
        changes: Sequence["OperationChange"],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> "OperationResult":
        from sandbox.occ.wire import (
            operation_change_to_dict,
            operation_result_from_dict,
        )

        result = self._call_sync(
            "occ.commit_against_base",
            {
                "workspace_root": self.workspace_root,
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
    ) -> list["OperationResult"]:
        from sandbox.occ.wire import operation_result_from_dict

        rows = self._call_sync(
            "occ.commit_many",
            {
                "workspace_root": self.workspace_root,
                "requests": list(requests),
            },
        )
        return [operation_result_from_dict(r) for r in (rows or [])]

    def write_file(
        self,
        specs: Sequence["WriteSpec"] | "WriteSpec",
        *,
        agent_id: str = "",
        description: str = "",
    ) -> "OperationResult":
        from sandbox.occ.wire import (
            normalize_write_specs,
            operation_result_from_dict,
            writespec_to_dict,
        )

        normalized = normalize_write_specs(specs)
        result = self._call_sync(
            "occ.write",
            {
                "workspace_root": self.workspace_root,
                "specs": [writespec_to_dict(s) for s in normalized],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

    def edit_file(
        self,
        specs: Sequence["EditSpec"] | "EditSpec",
        *,
        agent_id: str = "",
        description: str = "",
    ) -> "OperationResult":
        from sandbox.occ.wire import (
            editspec_to_dict,
            normalize_edit_specs,
            operation_result_from_dict,
        )

        normalized = normalize_edit_specs(specs)
        result = self._call_sync(
            "occ.edit",
            {
                "workspace_root": self.workspace_root,
                "specs": [editspec_to_dict(s) for s in normalized],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

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
