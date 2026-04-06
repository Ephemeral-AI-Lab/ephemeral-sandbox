"""Async Daytona SDK client wrapper.

Provides truly async sandbox access via AsyncDaytona with loop-aware
caching and proper cancellation support via asyncio.CancelledError.
"""

from __future__ import annotations

import asyncio
import logging
import os
import threading
from typing import Any

from sandbox.exc import AsyncDaytonaUnavailableError

logger = logging.getLogger(__name__)

_client_lock = threading.Lock()
_cached_client: Any | None = None
_cached_client_key: tuple[str, str, str] | None = None
_cached_loop_id: int | None = None


def _load_credentials() -> tuple[str, str, str]:
    api_key = os.environ.get("DAYTONA_API_KEY", "").strip()
    api_url = os.environ.get("DAYTONA_API_URL", "").strip()
    target = os.environ.get("DAYTONA_TARGET", "").strip()
    if not api_key or not api_url:
        raise AsyncDaytonaUnavailableError(
            "Async Daytona is not configured. Set DAYTONA_API_KEY and DAYTONA_API_URL env vars."
        )
    return api_key, api_url, target


def get_async_daytona_client() -> Any:
    """Return a cached AsyncDaytona client, creating one if config changed."""
    global _cached_client, _cached_client_key, _cached_loop_id
    loop = asyncio.get_running_loop()
    loop_id = id(loop)

    api_key, api_url, target = _load_credentials()
    current_key = (api_key, api_url, target)

    with _client_lock:
        if (
            _cached_client is not None
            and _cached_client_key == current_key
            and _cached_loop_id == loop_id
            and not loop.is_closed()
        ):
            return _cached_client

        if _cached_client is not None and _cached_loop_id != loop_id:
            from sandbox.lifecycle import close_client

            old_client = _cached_client
            _cached_client = None
            _cached_client_key = None
            _cached_loop_id = None
            close_client(old_client)

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
        _cached_client = AsyncDaytona(cfg)
        _cached_client_key = current_key
        _cached_loop_id = loop_id
        logger.info("AsyncDaytona client created (api_url=%s)", api_url)
        return _cached_client


async def get_async_sandbox(sandbox_id: str) -> Any:
    """Fetch and start a pre-created sandbox by ID using async client."""
    client = get_async_daytona_client()
    sandbox = await client.get(sandbox_id)
    if sandbox is None:
        raise ValueError(f"Sandbox '{sandbox_id}' not found")
    return sandbox
