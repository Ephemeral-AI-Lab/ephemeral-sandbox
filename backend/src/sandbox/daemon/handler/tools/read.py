"""``api.read_file`` dispatch entry."""

from __future__ import annotations

from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.workspace_binding import (
    WorkspaceBindingError,
    require_workspace_binding,
)
from sandbox.daemon.handler.request_context import (
    classify_path,
    layer_stack_root as require_layer_stack_root,
    required_single_path,
    services as backend_services,
)
from sandbox.timing import monotonic_now


async def read_file(args: dict[str, object]) -> dict[str, object]:
    """Single-path read_file dispatch with in/out-of-workspace classification."""
    total_start = monotonic_now()
    layer_stack_root = require_layer_stack_root(args)
    binding = require_workspace_binding(layer_stack_root)
    raw_path = required_single_path(args)
    classified = classify_path(raw_path, binding.workspace_root)

    if classified.classification == "out_of_workspace":
        return _read_out_of_workspace(
            abs_path=classified.abs_path,
            total_start=total_start,
        )

    return _read_in_workspace(
        layer_stack_root=layer_stack_root,
        layer_path=classified.layer_path,
        total_start=total_start,
    )


def _read_in_workspace(
    *,
    layer_stack_root: str,
    layer_path: str,
    total_start: float,
) -> dict[str, object]:
    services = backend_services(layer_stack_root)
    if not Path(layer_stack_root).exists():
        raise WorkspaceBindingError(
            f"layer-stack root does not exist: {layer_stack_root}"
        )
    request_id = uuid4().hex
    lease_start = monotonic_now()
    lease = services.manager.acquire_snapshot_lease(request_id)
    lease_acquired_s = monotonic_now() - lease_start
    try:
        read_start = monotonic_now()
        content, exists = services.layer_stack.read_text(layer_path, lease.manifest)
        read_elapsed = monotonic_now() - read_start
    finally:
        services.manager.release_lease(lease.lease_id)
    return {
        "success": True,
        "exists": exists,
        "content": content,
        "encoding": "utf-8",
        "timings": {
            "api.read.lease_acquire_s": lease_acquired_s,
            "api.read.layer_stack_read_s": read_elapsed,
            "api.read.total_s": monotonic_now() - total_start,
        },
    }


def _read_out_of_workspace(
    *,
    abs_path: str,
    total_start: float,
) -> dict[str, object]:
    target = Path(abs_path)
    if not target.exists():
        return {
            "success": True,
            "exists": False,
            "content": "",
            "encoding": "utf-8",
            "timings": {
                "api.read.total_s": monotonic_now() - total_start,
            },
        }
    # WR-01: cap out-of-workspace reads. Reading /var/log/syslog or
    # /proc/kcore unbounded OOMs the daemon and blows up the response
    # body. 16 MiB is comfortably larger than any legitimate source
    # file and small enough to never starve the daemon.
    _MAX_OUT_OF_WORKSPACE_READ_BYTES = 16 * 1024 * 1024
    try:
        size = target.stat().st_size
    except OSError:
        size = -1
    if size > _MAX_OUT_OF_WORKSPACE_READ_BYTES:
        raise ValueError(
            f"file too large: {size} > {_MAX_OUT_OF_WORKSPACE_READ_BYTES} bytes"
        )
    read_start = monotonic_now()
    content = target.read_text(encoding="utf-8")
    read_elapsed = monotonic_now() - read_start
    return {
        "success": True,
        "exists": True,
        "content": content,
        "encoding": "utf-8",
        "timings": {
            "api.read.host_fs_read_s": read_elapsed,
            "api.read.total_s": monotonic_now() - total_start,
        },
    }


__all__ = ["read_file"]
