"""``api.read_file`` dispatch entry."""

from __future__ import annotations

import time
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.workspace.binding import (
    WorkspaceBindingError,
    require_workspace_binding,
)
from sandbox.runtime.daemon.handler.request_context import (
    _layer_stack_root,
    _required_single_path,
    _services,
    classify_path,
)


async def read_file(args: dict[str, object]) -> dict[str, object]:
    """Single-path read_file dispatch with in/out-of-workspace classification."""
    total_start = time.perf_counter()
    layer_stack_root = _layer_stack_root(args)
    binding = require_workspace_binding(layer_stack_root)
    raw_path = _required_single_path(args)
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
    services = _services(layer_stack_root)
    if not Path(layer_stack_root).exists():
        raise WorkspaceBindingError(
            f"layer-stack root does not exist: {layer_stack_root}"
        )
    request_id = uuid4().hex
    lease_start = time.perf_counter()
    lease = services.manager.acquire_snapshot_lease(request_id)
    lease_acquired_s = time.perf_counter() - lease_start
    try:
        read_start = time.perf_counter()
        content, exists = services.layer_stack.read_text(layer_path, lease.manifest)
        read_elapsed = time.perf_counter() - read_start
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
            "api.read.total_s": time.perf_counter() - total_start,
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
                "api.read.total_s": time.perf_counter() - total_start,
            },
        }
    read_start = time.perf_counter()
    content = target.read_text(encoding="utf-8")
    read_elapsed = time.perf_counter() - read_start
    return {
        "success": True,
        "exists": True,
        "content": content,
        "encoding": "utf-8",
        "timings": {
            "api.read.host_fs_read_s": read_elapsed,
            "api.read.total_s": time.perf_counter() - total_start,
        },
    }


__all__ = ["read_file"]
