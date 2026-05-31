"""Daytona runtime-context preparation."""

from __future__ import annotations

import logging
from typing import Any

from sandbox.provider.daytona.adapter import DaytonaProviderAdapter
from sandbox.provider.daytona.workspace import (
    discover_workspace,
    discover_workspace_async,
    prepare_sandbox_runtime_context,
)
from sandbox.provider.registry import has_registered_adapter, register_adapter

logger = logging.getLogger(__name__)


class DaytonaContextPreparer:
    """Inject sandbox runtime state for sandbox tools."""

    def __init__(self, sandbox_id: str) -> None:
        self.sandbox_id = sandbox_id
        self._sandbox: Any | None = None
        self._sandbox_loop_id: int | None = None

    def _get_sandbox(self) -> Any:
        """Fetch the sync sandbox for the current preparation call."""
        if not self.sandbox_id:
            raise RuntimeError("No sandbox_id configured for tool context.")
        from sandbox.provider.daytona.client import fetch_sandbox as get_sandbox

        sandbox = get_sandbox(self.sandbox_id)
        self._sandbox = sandbox
        self._sandbox_loop_id = None
        logger.debug("Sandbox fetched: %s", self.sandbox_id)
        return sandbox

    async def _get_sandbox_async(self) -> Any:
        """Fetch the async sandbox once per event loop."""
        import asyncio

        loop_id = id(asyncio.get_running_loop())
        if self._sandbox is not None and self._sandbox_loop_id == loop_id:
            return self._sandbox
        self._sandbox = None
        self._sandbox_loop_id = None
        if not self.sandbox_id:
            raise RuntimeError("No sandbox_id configured for tool context.")
        from sandbox.provider.daytona.client import get_async_sandbox

        self._sandbox = await get_async_sandbox(self.sandbox_id)
        self._sandbox_loop_id = loop_id
        logger.debug("Async sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    def prepare_context(self, context: Any) -> None:
        """Add the sandbox and repo root to tool execution metadata."""
        sandbox = self._get_sandbox()
        repo_root = context.get("repo_root") or discover_workspace(sandbox)

        prepare_daytona_runtime_context(
            context,
            sandbox_id=self.sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )

    async def prepare_context_async(self, context: Any) -> None:
        """Add the async sandbox and repo root to tool execution metadata."""
        sandbox = await self._get_sandbox_async()
        repo_root = context.get("repo_root") or await discover_workspace_async(sandbox)

        prepare_daytona_runtime_context(
            context,
            sandbox_id=self.sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )


def prepare_daytona_runtime_context(
    context: Any,
    *,
    sandbox_id: str | None,
    sandbox: Any,
    workspace_root: str | None,
) -> None:
    """Inject provider-neutral runtime metadata and register the Daytona adapter."""

    prepare_sandbox_runtime_context(
        context,
        sandbox=sandbox,
        workspace_root=workspace_root,
    )

    if sandbox_id:
        try:
            if not has_registered_adapter(sandbox_id):
                register_adapter(sandbox_id, DaytonaProviderAdapter())
        except Exception:
            logger.debug(
                "Provider adapter attachment failed for sandbox %s",
                sandbox_id,
                exc_info=True,
            )


__all__ = [
    "DaytonaContextPreparer",
    "prepare_daytona_runtime_context",
]
