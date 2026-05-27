"""SWE-EVO docker snapshot register + verify (docker-only)."""

from __future__ import annotations

import logging
import subprocess

import sandbox.api as sandbox_api

from task_center_runner.benchmarks.sweevo.models import (
    SWEEvoInstance,
    _DEFAULT_SNAPSHOT_CREATE_TIMEOUT,
    _normalize_sweevo_image_ref,
    default_sweevo_snapshot_name,
)

logger = logging.getLogger(__name__)


class SnapshotNotRegisteredError(RuntimeError):
    """Raised when a SWE-EVO snapshot is missing.

    The CLI fails-fast on this so a missing snapshot surfaces before sandbox
    creation rather than after a 30-minute LLM run.
    """


def register_sweevo_snapshot(
    instance: SWEEvoInstance,
    *,
    snapshot_name: str = "",
) -> str:
    """Register a SWE-EVO Docker image as a provider snapshot (docker-only)."""
    if not instance.docker_image:
        raise ValueError(f"Instance {instance.instance_id} has no docker_image")

    name = snapshot_name or f"sweevo-{instance.instance_id_swe or instance.instance_id}"
    if len(name) > 63:
        name = name[:63]

    image_ref = _normalize_sweevo_image_ref(instance.docker_image)

    logger.info("Registering SWE-EVO snapshot '%s' from %s (docker)", name, image_ref)
    pull = subprocess.run(
        ["docker", "pull", image_ref],
        capture_output=True,
        text=True,
        timeout=_DEFAULT_SNAPSHOT_CREATE_TIMEOUT,
    )
    if pull.returncode != 0:
        raise RuntimeError(f"docker pull {image_ref} failed: {pull.stderr}")
    tag = subprocess.run(
        ["docker", "tag", image_ref, name],
        capture_output=True,
        text=True,
        timeout=_DEFAULT_SNAPSHOT_CREATE_TIMEOUT,
    )
    if tag.returncode != 0:
        raise RuntimeError(f"docker tag {image_ref} {name} failed: {tag.stderr}")
    logger.info("Registered snapshot: %s (docker)", name)
    return name


def resolve_sweevo_snapshot(
    instance: SWEEvoInstance,
    *,
    snapshot_name: str = "",
    register_snapshot: bool = True,
) -> str:
    """Resolve the snapshot identifier to use for a SWE-EVO sandbox."""
    if register_snapshot:
        return register_sweevo_snapshot(
            instance,
            snapshot_name=snapshot_name or default_sweevo_snapshot_name(instance),
        )
    return snapshot_name or instance.docker_image


def verify_sweevo_snapshot_exists(instance: SWEEvoInstance) -> str:
    """Assert the snapshot for *instance* exists.

    Returns the snapshot name on success. Raises
    :class:`SnapshotNotRegisteredError` if the snapshot is missing.

    Docker images are either present locally or not — there is no
    inactive/error state to normalize, so the check is a simple lookup.
    """
    name = default_sweevo_snapshot_name(instance)
    snapshots = sandbox_api.list_snapshots()
    match = next((s for s in snapshots if s.get("name") == name), None)
    if match is None:
        raise SnapshotNotRegisteredError(
            f"Snapshot {name!r} for instance {instance.instance_id!r} is not "
            f"registered. Pre-register it before invoking the CLI, e.g. by "
            f"calling task_center_runner.benchmarks.sweevo._snapshot."
            f"register_sweevo_snapshot(instance) from a Python shell."
        )
    return name


__all__ = [
    "SnapshotNotRegisteredError",
    "default_sweevo_snapshot_name",
    "register_sweevo_snapshot",
    "resolve_sweevo_snapshot",
    "verify_sweevo_snapshot_exists",
]
