"""Runtime handlers for layer-stack workspace binding operations."""

from __future__ import annotations

from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.daemon._toolbox import (
    layer_stack_root as require_layer_stack_root,
    require_arg,
)
from sandbox.daemon import workspace_server
from sandbox._shared.clock import monotonic_now


async def build_workspace_base(args: dict[str, object]) -> dict[str, object]:
    """Build (or rebuild on ``reset``) the layer-stack workspace base.

    ``reset=True`` drops peer runtime caches before rebuilding so the new
    base is rebound cleanly; that side effect is part of the public
    contract, not an internal optimization.
    """
    total_start = monotonic_now()
    layer_stack_root = require_layer_stack_root(args)
    reset = bool(args.get("reset", False))
    if reset:
        await _drop_peer_runtime_caches(layer_stack_root)
    timings: dict[str, float] = {}
    binding = workspace_server.build_workspace_base(
        layer_stack_root,
        workspace_root=require_arg(args, "workspace_root"),
        reset=reset,
        timings=timings,
    )
    return {
        "success": True,
        "created": True,
        "binding": binding.to_dict(),
        "timings": {
            **timings,
            "api.workspace_base.total_s": monotonic_now() - total_start,
        },
    }


async def ensure_workspace_base(args: dict[str, object]) -> dict[str, object]:
    total_start = monotonic_now()
    binding, created = workspace_server.ensure_workspace_base(
        require_layer_stack_root(args),
        workspace_root=require_arg(args, "workspace_root"),
    )
    return {
        "success": True,
        "created": created,
        "binding": binding.to_dict(),
        "timings": {
            "api.workspace_base.total_s": monotonic_now() - total_start,
        },
    }


async def workspace_binding(args: dict[str, object]) -> dict[str, object]:
    binding = require_workspace_binding(require_layer_stack_root(args))
    return {
        "success": True,
        "binding": binding.to_dict(),
    }


async def prepare_workspace_snapshot(args: dict[str, object]) -> dict[str, object]:
    total_start = monotonic_now()
    result = workspace_server.prepare_workspace_snapshot(
        require_layer_stack_root(args),
        owner_request_id=require_arg(args, "request_id"),
    )
    payload = result.to_dict()
    timings = payload.get("timings")
    if not isinstance(timings, dict):
        timings = {}
    payload["timings"] = {
        **timings,
        "api.prepare_workspace_snapshot.total_s": monotonic_now() - total_start,
    }
    return {
        "success": True,
        **payload,
    }


async def release_workspace_snapshot(args: dict[str, object]) -> dict[str, object]:
    released = workspace_server.release_workspace_snapshot(
        require_layer_stack_root(args),
        lease_id=require_arg(args, "lease_id"),
    )
    return {
        "success": True,
        "released": released,
    }


async def fence_stale_staging(args: dict[str, object]) -> dict[str, object]:
    return workspace_server.fence_stale_staging(require_layer_stack_root(args))


async def _drop_peer_runtime_caches(layer_stack_root: str) -> None:
    from sandbox.daemon import occ_backend

    occ_backend.drop_backend_cache(layer_stack_root)


__all__ = [
    "ensure_workspace_base",
    "build_workspace_base",
    "fence_stale_staging",
    "prepare_workspace_snapshot",
    "release_workspace_snapshot",
    "workspace_binding",
]
