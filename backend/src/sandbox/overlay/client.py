"""Host-side client for overlay runtime server operations."""

from __future__ import annotations

import uuid
from collections.abc import Mapping
from typing import Any

from sandbox.overlay.runner.snapshot_overlay_runner import SnapshotOverlayRunner
from sandbox.overlay.types import OverlayRunOutcome
from sandbox.overlay.types import OverlayShellRequest
from sandbox.overlay.wire import overlay_outcome_from_dict
from sandbox.providers.registry import get_adapter
from sandbox.runtime._server_dispatch import RuntimeDispatchError, call_runtime_server
from sandbox.runtime.overlay_shell.result_envelope import RuntimeResultEnvelope
from sandbox.runtime.types import ShellResult
from sandbox.runtime.wire import shell_result_from_dict


class OverlayClientError(RuntimeError):
    """Raised when the overlay runtime server returns an error envelope."""

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


class OverlayClient:
    """Typed host route for overlay requests through ``runtime/server.py``."""

    def __init__(
        self,
        sandbox_id: str | None = None,
        *,
        runner: SnapshotOverlayRunner | None = None,
        workspace_root: str = "/workspace",
        timeout: int = 300,
    ) -> None:
        if sandbox_id is None and runner is None:
            raise ValueError("sandbox_id or runner is required")
        self.sandbox_id = sandbox_id
        self._runner = runner
        self.workspace_root = workspace_root
        self.timeout = timeout

    async def shell_snapshot(
        self,
        request: OverlayShellRequest,
    ) -> RuntimeResultEnvelope:
        if self._runner is None:
            raise OverlayClientError(
                "MissingSnapshotRunner",
                "shell_snapshot requires a SnapshotOverlayRunner",
            )
        return await self._runner.shell(request)

    async def run_snapshot(
        self,
        command: tuple[str, ...],
        *,
        request_id: str | None = None,
        cwd: str = ".",
        env: Mapping[str, str] | None = None,
        timeout_seconds: float | None = None,
    ) -> RuntimeResultEnvelope:
        return await self.shell_snapshot(
            OverlayShellRequest(
                request_id=request_id or uuid.uuid4().hex,
                command=command,
                cwd=cwd,
                env=env or {},
                timeout_seconds=timeout_seconds,
            )
        )

    async def run(
        self,
        command: str,
        *,
        timeout: int | None = None,
        stdin: str | None = None,
        description: str = "",
        agent_id: str = "",
    ) -> OverlayRunOutcome:
        result = await self._call(
            "overlay.run",
            {
                "command": command,
                "timeout": timeout,
                "stdin": stdin,
                "description": description,
                "agent_id": agent_id,
            },
        )
        return overlay_outcome_from_dict(result)

    async def shell(
        self,
        command: str,
        *,
        timeout: int | None = None,
        stdin: str | None = None,
        description: str = "",
        agent_id: str = "",
    ) -> ShellResult:
        result = await self._call(
            "shell",
            {
                "command": command,
                "timeout": timeout,
                "stdin": stdin,
                "description": description,
                "agent_id": agent_id,
            },
        )
        return shell_result_from_dict(result)

    async def _call(self, op: str, args: dict[str, Any]) -> dict[str, Any]:
        if self.sandbox_id is None:
            raise OverlayClientError(
                "MissingSandboxId",
                "runtime-server overlay calls require sandbox_id",
            )
        try:
            return await call_runtime_server(
                exec_fn=get_adapter(self.sandbox_id).exec,
                sandbox_id=self.sandbox_id,
                op=op,
                args={
                    "workspace_root": self.workspace_root,
                    "sandbox_id": self.sandbox_id,
                    **args,
                },
                timeout=self.timeout,
            )
        except RuntimeDispatchError as exc:
            raise OverlayClientError(exc.kind, exc.message, exc.details) from exc


__all__ = ["OverlayClient", "OverlayClientError"]
