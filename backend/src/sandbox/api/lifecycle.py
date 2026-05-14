"""Public sandbox lifecycle/control verbs."""

from __future__ import annotations

from typing import Any

from sandbox.host import lifecycle as host_lifecycle


def configured_sandbox_defaults() -> tuple[str | None, str | None]:
    from config import load_settings

    sandbox = load_settings().sandbox
    snapshot = sandbox.default_snapshot.strip()
    image = sandbox.default_image.strip()
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


__all__ = [
    "create_sandbox",
    "delete_sandbox",
    "ensure_sandbox_running",
    "set_sandbox_labels",
    "start_sandbox",
    "stop_sandbox",
]
