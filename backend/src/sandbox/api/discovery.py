"""Public sandbox health and discovery verbs."""

from __future__ import annotations

from typing import Any

from sandbox.api.lifecycle import configured_sandbox_defaults
from sandbox.provider.registry import get_adapter, get_default_provider


def get_health() -> dict[str, Any]:
    """Provider connection health for the default adapter."""
    health = dict(get_default_provider().get_health())
    default_snapshot, default_image = configured_sandbox_defaults()
    health["default_snapshot"] = default_snapshot or health.get("default_snapshot")
    health["default_image"] = default_image or health.get("default_image")
    return health


def list_snapshots() -> list[dict[str, Any]]:
    """Snapshots available to the default provider."""
    return get_default_provider().list_snapshots()


def list_sandboxes() -> list[dict[str, Any]]:
    """All sandboxes the default provider can see."""
    return get_default_provider().list()


def get_sandbox(sandbox_id: str) -> dict[str, Any]:
    return get_adapter(sandbox_id).get(sandbox_id)


__all__ = [
    "get_health",
    "get_sandbox",
    "list_sandboxes",
    "list_snapshots",
]
