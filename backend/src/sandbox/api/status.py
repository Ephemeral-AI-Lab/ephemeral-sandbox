"""Public sandbox status and control verbs.

Single entry point for health checks, sandbox reads, and sandbox start/stop
control from outside the ``sandbox/`` package. The implementations route through
the registered :class:`~sandbox.provider.protocol.ProviderAdapter` (default +
per-id) and the provider-neutral :mod:`sandbox.host` orchestration.

All functions are sync and return plain dicts. Async callers (FastAPI route
handlers, the runtime agent) should dispatch them through
``sandbox.async_bridge.run_sync_in_executor``.
"""

from __future__ import annotations

from typing import Any

from sandbox.host.recovery import ensure_running as _ensure_running
from sandbox.host.setup import setup_after_create, setup_after_start
from sandbox.plugin import install as plugin_install
from sandbox.plugin import session as plugin_session
from sandbox.provider.registry import (
    dispose_adapter,
    get_adapter,
    get_default_provider,
    register_adapter,
)

# -- Health / discovery --------------------------------------------------------


def get_health() -> dict[str, Any]:
    """Provider connection health for the default adapter."""
    health = dict(get_default_provider().get_health())
    default_snapshot, default_image = _configured_sandbox_defaults()
    health["default_snapshot"] = default_snapshot or health.get("default_snapshot")
    health["default_image"] = default_image or health.get("default_image")
    return health


def list_snapshots() -> list[dict[str, Any]]:
    """Snapshots available to the default provider."""
    return get_default_provider().list_snapshots()


def list_sandboxes() -> list[dict[str, Any]]:
    """All sandboxes the default provider can see."""
    return get_default_provider().list()


# -- Per-sandbox reads ---------------------------------------------------------


def get_sandbox(sandbox_id: str) -> dict[str, Any]:
    return get_adapter(sandbox_id).get(sandbox_id)


def get_signed_preview_url(sandbox_id: str, port: int) -> dict[str, Any]:
    return get_adapter(sandbox_id).get_signed_preview_url(sandbox_id, port)


def get_build_logs_url(sandbox_id: str) -> str | None:
    return get_adapter(sandbox_id).get_build_logs_url(sandbox_id)


# -- Control -------------------------------------------------------------------


def create_sandbox(
    *,
    name: str,
    snapshot: str | None = None,
    image: str | None = None,
    language: str = "python",
    env_vars: dict[str, str] | None = None,
    labels: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Create a sandbox via the default provider, then run the post-create hook."""
    provider = get_default_provider()
    resolved_snapshot = snapshot
    resolved_image = image
    if not resolved_snapshot and not resolved_image:
        resolved_snapshot, resolved_image = _configured_sandbox_defaults()
    info = provider.create(
        name=name,
        snapshot=resolved_snapshot,
        image=resolved_image,
        language=language,
        env_vars=env_vars,
        labels=labels,
    )
    sandbox_id = info.get("id") or ""
    if sandbox_id:
        register_adapter(sandbox_id, provider)
        setup_after_create(sandbox_id, info.get("project_dir"))
    return info


def start_sandbox(sandbox_id: str) -> dict[str, Any]:
    """Start a stopped sandbox and run the post-start hook."""
    info = get_adapter(sandbox_id).start(sandbox_id)
    setup_after_start(sandbox_id, info.get("project_dir"))
    return info


def stop_sandbox(sandbox_id: str) -> dict[str, Any]:
    return get_adapter(sandbox_id).stop(sandbox_id)


def delete_sandbox(sandbox_id: str) -> None:
    get_adapter(sandbox_id).delete(sandbox_id)
    plugin_session.forget(sandbox_id)
    plugin_install.forget(sandbox_id)
    dispose_adapter(sandbox_id)


def set_sandbox_labels(sandbox_id: str, labels: dict[str, str]) -> dict[str, Any]:
    return get_adapter(sandbox_id).set_labels(sandbox_id, labels)


def ensure_sandbox_running(sandbox_id: str) -> dict[str, Any]:
    """Probe the sandbox; restart + re-run setup hook if the probe fails."""
    return _ensure_running(sandbox_id)


def _configured_sandbox_defaults() -> tuple[str | None, str | None]:
    from config import load_settings

    sandbox = load_settings().sandbox
    snapshot = sandbox.default_snapshot.strip()
    image = sandbox.default_image.strip()
    # Return both fields when configured. The caller picks precedence
    # (snapshot is preferred for warm starts, image is the cold-start
    # fallback). Pre-fix dropped image whenever snapshot was set, which
    # surprised get_health's fallback logic.
    return snapshot or None, image or None


__all__ = [
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
