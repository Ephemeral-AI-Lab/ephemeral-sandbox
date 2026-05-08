"""Async Daytona SDK client wrapper.

Provides truly async sandbox access via AsyncDaytona with loop-aware
caching and proper cancellation support via asyncio.CancelledError.
"""

from __future__ import annotations

import asyncio
import logging
import threading
import weakref
from typing import Any

from sandbox.provider.daytona.client.credentials import load_credentials
from sandbox.provider.daytona.errors import AsyncDaytonaUnavailableError
from sandbox.runtime.async_bridge import (
    register_standalone_loop_cleanup,
)

logger = logging.getLogger(__name__)

try:
    from sandbox.provider.daytona.client.shutdown import shutdown_cached_client_async

    register_standalone_loop_cleanup(shutdown_cached_client_async)
except Exception:
    logger.debug("Failed to register Daytona async-client cleanup", exc_info=True)

_client_lock = threading.Lock()
_cached_clients: weakref.WeakKeyDictionary[
    asyncio.AbstractEventLoop,
    tuple[tuple[str, str, str], Any],
] = weakref.WeakKeyDictionary()


def _load_credentials() -> tuple[str, str, str]:
    api_key, api_url, target = load_credentials()
    if not api_key or not api_url:
        raise AsyncDaytonaUnavailableError(
            "Async Daytona is not configured. Set DAYTONA_API_KEY and DAYTONA_API_URL."
        )
    return api_key, api_url, target


def get_async_daytona_client() -> Any:
    """Return a loop-local cached AsyncDaytona client.

    Concurrent EvalAgent tests run one event loop per thread. A single
    process-wide client causes one loop to close another loop's live
    transport, so cache one client per active loop instead.
    """
    loop = asyncio.get_running_loop()
    api_key, api_url, target = _load_credentials()
    current_key = (api_key, api_url, target)
    stale_clients: list[Any] = []

    with _client_lock:
        for cached_loop, (_, cached_client) in list(_cached_clients.items()):
            if cached_loop.is_closed():
                stale_clients.append(cached_client)
                del _cached_clients[cached_loop]

        cached_entry = _cached_clients.get(loop)
        if cached_entry is not None:
            cached_key, cached_client = cached_entry
            if cached_key == current_key and not loop.is_closed():
                return cached_client
            stale_clients.append(cached_client)
            del _cached_clients[loop]

        try:
            from daytona_sdk import AsyncDaytona, DaytonaConfig
        except ImportError as exc:
            raise AsyncDaytonaUnavailableError(
                "Async Daytona SDK is not available. Run: pip install daytona-sdk"
            ) from exc

        cfg_kwargs: dict[str, str] = {"api_key": api_key, "api_url": api_url}
        if target:
            cfg_kwargs["target"] = target
        cfg = DaytonaConfig(**cfg_kwargs)
        client = AsyncDaytona(cfg)
        _cached_clients[loop] = (current_key, client)

    if stale_clients:
        from sandbox.provider.daytona.client.shutdown import close_client

        for stale_client in stale_clients:
            close_client(stale_client)

    logger.info("AsyncDaytona client created (api_url=%s)", api_url)
    return client


async def get_async_sandbox(sandbox_id: str) -> Any:
    """Fetch and start a pre-created sandbox by ID using async client."""
    client = get_async_daytona_client()
    sandbox = await client.get(sandbox_id)
    if sandbox is None:
        raise ValueError(f"Sandbox '{sandbox_id}' not found")
    return sandbox
