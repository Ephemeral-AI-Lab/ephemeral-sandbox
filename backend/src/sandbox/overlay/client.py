"""Host-side client for overlay runtime server operations."""

from __future__ import annotations

from typing import Any

from sandbox.overlay.types import OverlayRunOutcome, ShellResult
from sandbox.overlay.wire import overlay_outcome_from_dict, shell_result_from_dict
from sandbox.providers.registry import get_adapter
from sandbox.runtime._server_dispatch import RuntimeDispatchError, call_runtime_server


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
        sandbox_id: str,
        *,
        workspace_root: str = "/workspace",
        timeout: int = 300,
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self.timeout = timeout

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
