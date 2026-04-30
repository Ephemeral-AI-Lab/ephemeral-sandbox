"""Daytona credentials — API key, URL, and target resolution."""

from __future__ import annotations

import os


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
