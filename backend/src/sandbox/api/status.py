"""Compatibility facade for public sandbox health, lifecycle, and URL verbs."""

from __future__ import annotations

from sandbox.api.defaults import configured_sandbox_defaults as _configured_sandbox_defaults
from sandbox.api.discovery import (
    get_health,
    get_sandbox,
    list_sandboxes,
    list_snapshots,
)
from sandbox.api.lifecycle import (
    create_sandbox,
    delete_sandbox,
    ensure_sandbox_running,
    set_sandbox_labels,
    start_sandbox,
    stop_sandbox,
)
from sandbox.api.preview_urls import get_build_logs_url, get_signed_preview_url

__all__ = [
    "_configured_sandbox_defaults",
    "create_sandbox",
    "delete_sandbox",
    "ensure_sandbox_running",
    "get_build_logs_url",
    "get_health",
    "get_sandbox",
    "get_signed_preview_url",
    "list_sandboxes",
    "list_snapshots",
    "set_sandbox_labels",
    "start_sandbox",
    "stop_sandbox",
]
