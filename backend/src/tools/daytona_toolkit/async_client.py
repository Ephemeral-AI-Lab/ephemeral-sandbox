"""Async Daytona SDK client wrapper — async initialization and caching.

This module provides async access to Daytona sandboxes using AsyncDaytona,
which returns AsyncSandbox objects with truly async methods (AsyncProcess,
AsyncFileSystem, etc.) that can be properly cancelled via asyncio.CancelledError.
"""

from __future__ import annotations

import asyncio
import logging
import os
import threading
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from daytona_sdk import DaytonaConfig

logger = logging.getLogger(__name__)

_client_lock = threading.Lock()
_cached_client: Any | None = None
_cached_client_key: tuple[str, str, str] | None = None


class AsyncDaytonaUnavailableError(RuntimeError):
    """Raised when Async Daytona SDK is not installed or not configured."""


def _require_settings() -> tuple[str, str, str]:
    """Return (api_key, api_url, target) from env vars."""
    api_key = os.environ.get("DAYTONA_API_KEY", "").strip()
    api_url = os.environ.get("DAYTONA_API_URL", "").strip()
    target = os.environ.get("DAYTONA_TARGET", "").strip()

    if not api_key or not api_url:
        raise AsyncDaytonaUnavailableError(
            "Async Daytona is not configured. Set DAYTONA_API_KEY and DAYTONA_API_URL env vars."
        )
    return api_key, api_url, target


def _get_daytona_config() -> "DaytonaConfig":
    """Build DaytonaConfig from settings."""
    api_key, api_url, target = _require_settings()

    try:
        from daytona_sdk import DaytonaConfig
    except ImportError as exc:
        raise AsyncDaytonaUnavailableError(
            "Daytona SDK is not installed. Run: pip install daytona-sdk"
        ) from exc

    cfg_kwargs: dict[str, str] = {"api_key": api_key, "api_url": api_url}
    if target:
        cfg_kwargs["target"] = target
    return DaytonaConfig(**cfg_kwargs)


def get_async_daytona_client() -> Any:
    """Return a cached AsyncDaytona client, creating one if config changed."""
    global _cached_client, _cached_client_key

    api_key, api_url, target = _require_settings()
    current_key = (api_key, api_url, target)

    with _client_lock:
        if _cached_client is not None and _cached_client_key == current_key:
            return _cached_client

        try:
            from daytona_sdk import AsyncDaytona
        except ImportError as exc:
            raise AsyncDaytonaUnavailableError(
                "Async Daytona SDK is not available. Run: pip install daytona-sdk"
            ) from exc

        cfg = _get_daytona_config()
        _cached_client = AsyncDaytona(cfg)
        _cached_client_key = current_key
        logger.info("AsyncDaytona client created (api_url=%s)", api_url)
        return _cached_client


async def get_async_sandbox(sandbox_id: str) -> Any:
    """Fetch and start a pre-created sandbox by ID using async client.

    Returns an AsyncSandbox with async process, fs, and git interfaces
    that support proper cancellation via asyncio.CancelledError.
    """
    client = get_async_daytona_client()
    sandbox = await client.get(sandbox_id)
    if sandbox is None:
        raise ValueError(f"Sandbox '{sandbox_id}' not found")
    return sandbox
