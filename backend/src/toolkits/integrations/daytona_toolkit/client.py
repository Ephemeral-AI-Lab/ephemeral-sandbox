"""Daytona SDK client wrapper — lazy initialization and caching."""

from __future__ import annotations

import logging
import os
import threading
from typing import Any

logger = logging.getLogger(__name__)

_client_lock = threading.Lock()
_cached_client: Any | None = None
_cached_client_key: tuple[str, str, str] | None = None


class DaytonaUnavailableError(RuntimeError):
    """Raised when Daytona SDK is not installed or not configured."""


def _require_settings() -> tuple[str, str, str]:
    """Return (api_key, api_url, target) from persisted settings or env vars."""
    # Try persisted settings first
    try:
        from ephemeralos.config import load_settings
        settings = load_settings()
        api_key = settings.daytona_api_key.strip()
        api_url = settings.daytona_api_url.strip()
        target = settings.daytona_target.strip()
    except Exception:
        api_key = api_url = target = ""

    # Fall back to environment variables
    if not api_key:
        api_key = os.environ.get("DAYTONA_API_KEY", "").strip()
    if not api_url:
        api_url = os.environ.get("DAYTONA_API_URL", "").strip()
    if not target:
        target = os.environ.get("DAYTONA_TARGET", "").strip()

    if not api_key or not api_url:
        raise DaytonaUnavailableError(
            "Daytona is not configured. Set daytona_api_key and daytona_api_url "
            "in settings.json, or DAYTONA_API_KEY and DAYTONA_API_URL env vars."
        )
    return api_key, api_url, target


def get_daytona_client() -> Any:
    """Return a cached Daytona client, creating one if config changed."""
    global _cached_client, _cached_client_key

    api_key, api_url, target = _require_settings()
    current_key = (api_key, api_url, target)

    with _client_lock:
        if _cached_client is not None and _cached_client_key == current_key:
            return _cached_client

        try:
            from daytona_sdk import Daytona, DaytonaConfig
        except ImportError as exc:
            raise DaytonaUnavailableError(
                "Daytona SDK is not installed. Run: pip install daytona-sdk"
            ) from exc

        cfg_kwargs: dict[str, str] = {"api_key": api_key, "api_url": api_url}
        if target:
            cfg_kwargs["target"] = target
        cfg = DaytonaConfig(**cfg_kwargs)
        _cached_client = Daytona(cfg)
        _cached_client_key = current_key
        logger.info("Daytona client created (api_url=%s)", api_url)
        return _cached_client


def get_sandbox(sandbox_id: str) -> Any:
    """Fetch and start a pre-created sandbox by ID."""
    client = get_daytona_client()
    sandbox = client.get(sandbox_id)
    if sandbox is None:
        raise ValueError(f"Sandbox '{sandbox_id}' not found")
    return sandbox
