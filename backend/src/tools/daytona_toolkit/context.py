"""Daytona execution-context preparation."""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)


class DaytonaContextPreparer:
    """Inject sandbox and code-intelligence runtime state for Daytona-backed tools."""

    def __init__(self, sandbox_id: str) -> None:
        self.sandbox_id = sandbox_id
        self._sandbox: Any | None = None
        self._sandbox_loop_id: int | None = None

    def _get_sandbox(self) -> Any:
        """Fetch the sync sandbox once and cache it."""
        if self._sandbox is not None:
            return self._sandbox
        if not self.sandbox_id:
            raise RuntimeError("No sandbox_id configured for Daytona tool context.")
        from sandbox import fetch_sandbox as get_sandbox

        self._sandbox = get_sandbox(self.sandbox_id)
        logger.debug("Daytona sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    async def _get_sandbox_async(self) -> Any:
        """Fetch the async sandbox once per event loop."""
        import asyncio

        loop_id = id(asyncio.get_running_loop())
        if self._sandbox is not None and self._sandbox_loop_id == loop_id:
            return self._sandbox
        self._sandbox = None
        self._sandbox_loop_id = None
        if not self.sandbox_id:
            raise RuntimeError("No sandbox_id configured for Daytona tool context.")
        from sandbox.async_client import get_async_sandbox

        self._sandbox = await get_async_sandbox(self.sandbox_id)
        self._sandbox_loop_id = loop_id
        logger.debug("Async Daytona sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    @staticmethod
    def _resolve_cwd_sync(sandbox: Any) -> str | None:
        from sandbox.workspace import discover_workspace

        return discover_workspace(sandbox)

    @staticmethod
    async def _resolve_cwd_async(sandbox: Any) -> str | None:
        from sandbox.workspace import discover_workspace_async

        return await discover_workspace_async(sandbox)

    def prepare_context(self, context: Any) -> None:
        """Add the sandbox and repo root to tool execution metadata."""
        sandbox = self._get_sandbox()
        repo_root = context.metadata.get("repo_root") or self._resolve_cwd_sync(sandbox)
        from sandbox.workspace import ensure_code_intelligence_runtime

        ensure_code_intelligence_runtime(
            context,
            sandbox_id=self.sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )

    async def prepare_context_async(self, context: Any) -> None:
        """Add the async sandbox and repo root to tool execution metadata."""
        sandbox = await self._get_sandbox_async()
        repo_root = context.metadata.get("repo_root") or await self._resolve_cwd_async(sandbox)
        from sandbox.workspace import ensure_code_intelligence_runtime

        ensure_code_intelligence_runtime(
            context,
            sandbox_id=self.sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )
