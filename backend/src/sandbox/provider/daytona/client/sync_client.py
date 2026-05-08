"""Daytona sync client cache + helpers shared with sandbox/lifecycle."""

from __future__ import annotations

import logging
import os
import threading
from typing import Any

from sandbox.provider.daytona.client.credentials import load_credentials
from sandbox.provider.daytona.errors import DaytonaUnavailableError

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Labels & constants shared by the Daytona adapter.
# ---------------------------------------------------------------------------

_APP_MANAGED_BY = "ephemeralos"
_APP_CREATED_VIA = "api"
_SNAPSHOT_LABEL = "ephemeralos_snapshot"
_IMAGE_LABEL = "ephemeralos_image"
_LIST_PAGE_LIMIT = 100
_SNAPSHOT_PAGE_LIMIT = 100


def _timeout_seconds_from_env() -> float:
    raw = os.getenv("EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS")
    if not raw:
        return 300.0
    try:
        value = float(raw)
    except ValueError:
        logger.warning("Invalid EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS=%r; using default", raw)
        return 300.0
    return max(value, 1.0)


_SANDBOX_TIMEOUT_SECONDS = _timeout_seconds_from_env()


# ---------------------------------------------------------------------------
# Client lifecycle
# ---------------------------------------------------------------------------

_client_lock = threading.Lock()
_cached_client: Any | None = None
_cached_client_key: tuple[str, str, str] | None = None


def acquire_client() -> Any:
    """Return a cached Daytona client, creating one if config changed."""
    global _cached_client, _cached_client_key

    api_key, api_url, target = load_credentials()
    if not api_key or not api_url:
        raise DaytonaUnavailableError(
            "Daytona is not configured. Set DAYTONA_API_KEY and DAYTONA_API_URL."
        )
    current_key = (api_key, api_url, target)

    with _client_lock:
        if _cached_client is not None and _cached_client_key == current_key:
            return _cached_client

        try:
            from daytona_sdk import Daytona, DaytonaConfig
        except ImportError as exc:
            raise DaytonaUnavailableError(
                "Daytona SDK not installed. Run: pip install daytona-sdk"
            ) from exc

        cfg_kwargs: dict[str, str] = {"api_key": api_key, "api_url": api_url}
        if target:
            cfg_kwargs["target"] = target
        cfg = DaytonaConfig(**cfg_kwargs)
        _cached_client = Daytona(cfg)
        _cached_client_key = current_key
        logger.info("Daytona client created (api_url=%s)", api_url)
        return _cached_client


def fetch_sandbox(sandbox_id: str) -> Any:
    """Fetch a pre-created sandbox by ID."""
    client = acquire_client()
    sandbox = client.get(sandbox_id)
    if sandbox is None:
        raise ValueError(f"Sandbox '{sandbox_id}' not found")
    return sandbox


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def _normalize_optional_text(value: str | None) -> str | None:
    if value is None:
        return None
    normalized = value.strip()
    return normalized or None


def _normalize_dict(payload: dict[str, str] | None) -> dict[str, str]:
    if not payload:
        return {}
    return {str(k).strip(): str(v).strip() for k, v in payload.items() if str(k).strip()}


def _daytona_classes():
    """Import and return Daytona SDK classes."""
    try:
        from daytona_sdk import (
            CreateSandboxFromImageParams,
            CreateSandboxFromSnapshotParams,
            Daytona,
            DaytonaConfig,
        )
    except ImportError:
        try:
            from daytona import (
                CreateSandboxFromImageParams,
                CreateSandboxFromSnapshotParams,
                Daytona,
                DaytonaConfig,
            )
        except ImportError as exc:
            raise DaytonaUnavailableError(
                "Daytona SDK not installed. Run: pip install daytona-sdk"
            ) from exc

    return Daytona, DaytonaConfig, CreateSandboxFromSnapshotParams, CreateSandboxFromImageParams


def _paginate_all(list_fn: Any, limit: int) -> list[Any]:
    """Exhaust a paginated Daytona SDK list method and return all items."""
    first_page = list_fn(limit=limit)
    items = list(getattr(first_page, "items", []) or [])
    current_page = int(getattr(first_page, "page", 1) or 1)
    total_pages = int(getattr(first_page, "total_pages", 1) or 1)
    for page in range(current_page + 1, total_pages + 1):
        response = list_fn(page=page, limit=limit)
        items.extend(list(getattr(response, "items", []) or []))
    return items


__all__ = [
    "acquire_client",
    "fetch_sandbox",
]
