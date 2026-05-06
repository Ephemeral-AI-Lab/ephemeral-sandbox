"""Runtime handlers for layer-stack workspace binding operations."""

from __future__ import annotations

import time
from collections.abc import Mapping

from sandbox.layer_stack.workspace import require_workspace_binding
from sandbox.runtime.layer_stack_server import LayerStackWorkspaceServer


async def build_workspace_base(args: dict[str, object]) -> dict[str, object]:
    total_start = time.perf_counter()
    server = _server(args)
    binding = server.build_workspace_base(
        workspace_root=_workspace_root(args),
        reset=bool(args.get("reset", False)),
    )
    return {
        "success": True,
        "created": True,
        "binding": binding.to_dict(),
        "timings": {
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


def _server(args: Mapping[str, object]) -> LayerStackWorkspaceServer:
    return LayerStackWorkspaceServer(_layer_stack_root(args))


def _layer_stack_root(args: Mapping[str, object]) -> str:
    layer_stack_root = str(args.get("layer_stack_root") or "").strip()
    if not layer_stack_root:
        raise ValueError("layer_stack_root is required")
    return layer_stack_root


def _workspace_root(args: Mapping[str, object]) -> str:
    workspace_root = str(args.get("workspace_root") or "").strip()
    if not workspace_root:
        raise ValueError("workspace_root is required")
    return workspace_root


__all__ = [
    "ensure_workspace_base",
    "build_workspace_base",
    "workspace_binding",
]
