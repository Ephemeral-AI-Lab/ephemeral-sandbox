"""Provider-backed sandbox control-plane verbs."""

from __future__ import annotations

from typing import Any

from sandbox.host import lifecycle as host_lifecycle
from sandbox.provider.registry import get_adapter, get_default_provider


def configured_sandbox_defaults() -> tuple[str | None, str | None]:
    from config import get_central_config

    sandbox_config = get_central_config().sandbox
    if sandbox_config.default_provider == "daytona":
        snapshot = sandbox_config.daytona.default_snapshot.strip()
        image = sandbox_config.daytona.default_image.strip()
    else:
        snapshot = sandbox_config.docker.default_snapshot.strip()
        image = ""
    return snapshot or None, image or None


def create_sandbox(
    *,
    name: str,
    snapshot: str | None = None,
    image: str | None = None,
    language: str = "python",
    env_vars: dict[str, str] | None = None,
    labels: dict[str, str] | None = None,
) -> dict[str, Any]:
    resolved_snapshot = snapshot
    resolved_image = image
    if not resolved_snapshot and not resolved_image:
        resolved_snapshot, resolved_image = configured_sandbox_defaults()
    return host_lifecycle.create_sandbox(
        name=name,
        snapshot=resolved_snapshot,
        image=resolved_image,
        language=language,
        env_vars=env_vars,
        labels=labels,
    )


def start_sandbox(sandbox_id: str) -> dict[str, Any]:
    return host_lifecycle.start_sandbox(sandbox_id)


def stop_sandbox(sandbox_id: str) -> dict[str, Any]:
    return host_lifecycle.stop_sandbox(sandbox_id)


def delete_sandbox(sandbox_id: str) -> None:
    host_lifecycle.delete_sandbox(sandbox_id)


def ensure_sandbox_running(sandbox_id: str) -> dict[str, Any]:
    return host_lifecycle.ensure_sandbox_running(sandbox_id)


def set_sandbox_labels(sandbox_id: str, labels: dict[str, str]) -> dict[str, Any]:
    return host_lifecycle.set_sandbox_labels(sandbox_id, labels)


def get_health() -> dict[str, Any]:
    """Provider connection health for the default adapter."""
    health = dict(get_default_provider().get_health())
    default_snapshot, default_image = configured_sandbox_defaults()
    health["default_snapshot"] = default_snapshot or health.get("default_snapshot")
    health["default_image"] = default_image or health.get("default_image")
    return health


def list_snapshots() -> list[dict[str, Any]]:
    return get_default_provider().list_snapshots()


def list_sandboxes() -> list[dict[str, Any]]:
    return get_default_provider().list()


def get_sandbox(sandbox_id: str) -> dict[str, Any]:
    return get_adapter(sandbox_id).get(sandbox_id)


def get_signed_preview_url(sandbox_id: str, port: int) -> dict[str, Any]:
    return get_adapter(sandbox_id).get_signed_preview_url(sandbox_id, port)


def get_build_logs_url(sandbox_id: str) -> str | None:
    return get_adapter(sandbox_id).get_build_logs_url(sandbox_id)


def context_preparer_for(sandbox_id: str) -> Any:
    """Return the provider-owned context preparer for *sandbox_id*."""
    adapter = get_adapter(sandbox_id)
    factory = getattr(adapter, "context_preparer", None)
    if not callable(factory):
        raise RuntimeError(
            f"Provider adapter for sandbox {sandbox_id!r} does not expose context_preparer()."
        )
    return factory(sandbox_id)


__all__ = [
    "configured_sandbox_defaults",
    "context_preparer_for",
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
