"""Daytona credentials — API key, URL, and target resolution."""

from __future__ import annotations

import os
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from daytona_sdk import DaytonaConfig


def load_credentials() -> tuple[str, str, str]:
    api_key: str
    api_url: str
    target: str

    try:
        from config import load_settings

        settings = load_settings()
        api_key = (settings.daytona_api_key or "").strip()
        api_url = (settings.daytona_api_url or "").strip()
        target = (settings.daytona_target or "").strip()
    except Exception:
        api_key = api_url = target = ""

    if not api_key:
        api_key = os.environ.get("DAYTONA_API_KEY", "").strip()
    if not api_url:
        api_url = os.environ.get("DAYTONA_API_URL", "").strip()
    if not target:
        target = os.environ.get("DAYTONA_TARGET", "").strip()

    return api_key, api_url, target


def build_config() -> "DaytonaConfig":
    api_key, api_url, target = load_credentials()

    if not api_key or not api_url:
        from sandbox.exc import DaytonaUnavailableError

        raise DaytonaUnavailableError(
            "Daytona is not configured. Set daytona_api_key and daytona_api_url "
            "in settings.json, or DAYTONA_API_KEY and DAYTONA_API_URL env vars."
        )

    try:
        from daytona_sdk import DaytonaConfig
    except ImportError as exc:
        from sandbox.exc import DaytonaUnavailableError

        raise DaytonaUnavailableError(
            "Daytona SDK is not installed. Run: pip install daytona-sdk"
        ) from exc

    cfg_kwargs: dict[str, str] = {"api_key": api_key, "api_url": api_url}
    if target:
        cfg_kwargs["target"] = target
    return DaytonaConfig(**cfg_kwargs)
