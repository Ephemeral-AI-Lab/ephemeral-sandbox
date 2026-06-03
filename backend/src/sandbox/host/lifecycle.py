"""Provider-neutral sandbox lifecycle orchestration."""

from __future__ import annotations

from typing import Any

from sandbox.host.bootstrap import ensure_running as _ensure_running
from sandbox.host.bootstrap import setup_after_create, setup_after_start
from sandbox.api import plugin_dispatch as plugin_host_dispatch
from sandbox.api import plugin_install
from sandbox.provider.registry import (
    dispose_adapter,
    get_adapter,
    get_default_provider,
    register_adapter,
)


def create_sandbox(
    *,
    name: str,
    snapshot: str | None = None,
    image: str | None = None,
    language: str = "python",
    env_vars: dict[str, str] | None = None,
    labels: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Create a sandbox via the default provider, then run post-create setup."""
    provider = get_default_provider()
    info = provider.create(
        name=name,
        snapshot=snapshot,
        image=image,
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
    """Start a stopped sandbox and run post-start setup."""
    info = get_adapter(sandbox_id).start(sandbox_id)
    setup_after_start(sandbox_id, info.get("project_dir"))
    return info


def stop_sandbox(sandbox_id: str) -> dict[str, Any]:
    return get_adapter(sandbox_id).stop(sandbox_id)


def delete_sandbox(sandbox_id: str) -> None:
    get_adapter(sandbox_id).delete(sandbox_id)
    plugin_host_dispatch.forget_plugin_dispatch_state(sandbox_id)
    plugin_install.forget_plugin_install_state(sandbox_id)
    dispose_adapter(sandbox_id)


def set_sandbox_labels(sandbox_id: str, labels: dict[str, str]) -> dict[str, Any]:
    return get_adapter(sandbox_id).set_labels(sandbox_id, labels)


def ensure_sandbox_running(sandbox_id: str) -> dict[str, Any]:
    return _ensure_running(sandbox_id)


__all__ = [
    "create_sandbox",
    "delete_sandbox",
    "ensure_sandbox_running",
    "plugin_install",
    "plugin_host_dispatch",
    "set_sandbox_labels",
    "setup_after_create",
    "setup_after_start",
    "start_sandbox",
    "stop_sandbox",
]
