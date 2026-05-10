"""Runtime handlers for layer-stack workspace binding operations."""

from __future__ import annotations

import time
from collections.abc import Mapping

from sandbox.layer_stack.workspace.binding import require_workspace_binding
from sandbox.runtime.daemon.service.workspace_server import (
    LayerStackWorkspaceServer,
    fence_stale_staging as fence_stale_staging_for_root,
)


async def build_workspace_base(args: dict[str, object]) -> dict[str, object]:
    total_start = time.perf_counter()
    layer_stack_root = _layer_stack_root(args)
    reset = bool(args.get("reset", False))
    if reset:
        await _drop_peer_runtime_caches(layer_stack_root)
    server = LayerStackWorkspaceServer(layer_stack_root)
    timings: dict[str, float] = {}
    binding = server.build_workspace_base(
        workspace_root=_workspace_root(args),
        reset=reset,
        timings=timings,
    )
    return {
        "success": True,
        "created": True,
        "binding": binding.to_dict(),
        "timings": {
            **timings,
            "api.workspace_base.total_s": time.perf_counter() - total_start,
        },
    }


async def ensure_workspace_base(args: dict[str, object]) -> dict[str, object]:
    total_start = time.perf_counter()
    server = _server(args)
    binding, created = server.ensure_workspace_base(
        workspace_root=_workspace_root(args),
    )
    return {
        "success": True,
        "created": created,
        "binding": binding.to_dict(),
        "timings": {
            "api.workspace_base.total_s": time.perf_counter() - total_start,
        },
    }


async def workspace_binding(args: dict[str, object]) -> dict[str, object]:
    binding = require_workspace_binding(_layer_stack_root(args))
    return {
        "success": True,
        "binding": binding.to_dict(),
    }


async def prepare_workspace_snapshot(args: dict[str, object]) -> dict[str, object]:
    total_start = time.perf_counter()
    server = _server(args)
    result = server.prepare_workspace_snapshot(
        owner_request_id=_owner_request_id(args),
    )
    payload = result.to_dict()
    timings = payload.get("timings")
    if not isinstance(timings, dict):
        timings = {}
    payload["timings"] = {
        **timings,
        "api.prepare_workspace_snapshot.total_s": time.perf_counter() - total_start,
    }
    return {
        "success": True,
        **payload,
    }


async def release_workspace_snapshot(args: dict[str, object]) -> dict[str, object]:
    server = _server(args)
    released = server.release_workspace_snapshot(lease_id=_lease_id(args))
    return {
        "success": True,
        "released": released,
    }


async def fence_stale_staging(args: dict[str, object]) -> dict[str, object]:
    return fence_stale_staging_for_root(_layer_stack_root(args))


def _server(args: Mapping[str, object]) -> LayerStackWorkspaceServer:
    return LayerStackWorkspaceServer(_layer_stack_root(args))


def _required_str(args: Mapping[str, object], key: str) -> str:
    value = str(args.get(key) or "").strip()
    if not value:
        raise ValueError(f"{key} is required")
    return value


def _layer_stack_root(args: Mapping[str, object]) -> str:
    return _required_str(args, "layer_stack_root")


def _workspace_root(args: Mapping[str, object]) -> str:
    return _required_str(args, "workspace_root")


def _owner_request_id(args: Mapping[str, object]) -> str:
    return _required_str(args, "request_id")


def _lease_id(args: Mapping[str, object]) -> str:
    return _required_str(args, "lease_id")


async def _drop_peer_runtime_caches(layer_stack_root: str) -> None:
    from sandbox.runtime.daemon.service import occ_backend

    await occ_backend.drain_backend_auto_squash(layer_stack_root)
    occ_backend.drop_backend_cache(layer_stack_root)


__all__ = [
    "ensure_workspace_base",
    "build_workspace_base",
    "fence_stale_staging",
    "prepare_workspace_snapshot",
    "release_workspace_snapshot",
    "workspace_binding",
]
