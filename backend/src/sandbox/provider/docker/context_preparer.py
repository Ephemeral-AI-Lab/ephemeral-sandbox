"""Docker runtime-context preparation.

Mirrors the public surface of
:class:`sandbox.provider.daytona.context_preparer.DaytonaContextPreparer` so call
sites in ``sandbox/api/provider_control.py`` are symmetric.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from sandbox.provider.docker.adapter import DockerProviderAdapter
from sandbox.provider.docker.client import get_async_docker_client, get_docker_client
from sandbox.provider.docker.workspace import (
    discover_workspace,
    discover_workspace_async,
    prepare_sandbox_runtime_context,
)
from sandbox.provider.registry import has_registered_adapter, register_adapter

logger = logging.getLogger(__name__)


class DockerContextPreparer:
    """Inject Docker container runtime state for sandbox tools."""

    def __init__(self, sandbox_id: str) -> None:
        self.sandbox_id = sandbox_id
        self._container: Any | None = None
        self._container_loop_id: int | None = None

    def _get_container(self) -> Any:
        if not self.sandbox_id:
            raise RuntimeError("No sandbox_id configured for tool context.")
        client = get_docker_client()
        container = client.containers.get(self.sandbox_id)
        container.reload()
        self._container = container
        self._container_loop_id = None
        logger.debug("Docker container fetched: %s", self.sandbox_id)
        return container

    async def _get_container_async(self) -> Any:
        loop_id = id(asyncio.get_running_loop())
        if self._container is not None and self._container_loop_id == loop_id:
            return self._container
        self._container = None
        self._container_loop_id = None
        if not self.sandbox_id:
            raise RuntimeError("No sandbox_id configured for tool context.")

        def _fetch() -> Any:
            client = get_async_docker_client()
            container = client.containers.get(self.sandbox_id)
            container.reload()
            return container

        self._container = await asyncio.to_thread(_fetch)
        self._container_loop_id = loop_id
        logger.debug("Async docker container fetched: %s", self.sandbox_id)
        return self._container

    def prepare_context(self, context: Any) -> None:
        container = self._get_container()
        repo_root = context.get("repo_root") or discover_workspace(container)
        prepare_docker_runtime_context(
            context,
            sandbox_id=self.sandbox_id,
            container=container,
            workspace_root=repo_root,
        )

    async def prepare_context_async(self, context: Any) -> None:
        container = await self._get_container_async()
        repo_root = context.get("repo_root") or await discover_workspace_async(container)
        prepare_docker_runtime_context(
            context,
            sandbox_id=self.sandbox_id,
            container=container,
            workspace_root=repo_root,
        )


def prepare_docker_runtime_context(
    context: Any,
    *,
    sandbox_id: str | None,
    container: Any,
    workspace_root: str | None,
) -> None:
    """Inject provider-neutral runtime metadata and register the Docker adapter."""
    prepare_sandbox_runtime_context(
        context,
        container=container,
        workspace_root=workspace_root,
    )

    if sandbox_id:
        try:
            if not has_registered_adapter(sandbox_id):
                register_adapter(sandbox_id, DockerProviderAdapter())
        except Exception:
            logger.debug(
                "Provider adapter attachment failed for sandbox %s",
                sandbox_id,
                exc_info=True,
            )


__all__ = [
    "DockerContextPreparer",
    "prepare_docker_runtime_context",
]
