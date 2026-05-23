"""``api.glob`` dispatch entry (read-only).

Enumerates snapshot paths matching a fnmatch glob pattern. Acquires a
snapshot lease (MVCC read isolation, mirrors ``read.py``) and walks paths
through ``services.layer_stack`` against ``lease.manifest`` so the scan is
consistent with the leased snapshot. The handler does not import
``occ_client`` or touch the OCC mutation gate — the surface is read-only by
construction.

Shares the directory-walk helpers (``is_vcs_excluded``, ``layer_subpath``,
``under``) with :mod:`sandbox.daemon.handler.grep`.
"""

from __future__ import annotations

import fnmatch
from typing import Any
from uuid import uuid4

from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.daemon.async_bridge import run_sync_in_executor
from sandbox.daemon.occ_backend import build_occ_backend
from sandbox.daemon.request_context import require_layer_stack_root
from sandbox._shared.clock import monotonic_now

from sandbox.daemon.handler.grep import (
    is_vcs_excluded,
    layer_subpath,
    under,
)


DEFAULT_GLOB_LIMIT = 100


def _glob_sync(args: dict[str, Any]) -> dict[str, Any]:
    total_start = monotonic_now()
    layer_stack_root = require_layer_stack_root(args)
    binding = require_workspace_binding(layer_stack_root)
    pattern = str(args.get("pattern") or "").strip()
    if not pattern:
        raise ValueError("pattern is required")
    sub_path = layer_subpath(args, binding.workspace_root)

    services = build_occ_backend(layer_stack_root)
    request_id = uuid4().hex
    lease_start = monotonic_now()
    lease = services.manager.acquire_snapshot_lease(request_id)
    lease_acquired_s = monotonic_now() - lease_start
    try:
        iter_start = monotonic_now()
        matches: list[str] = []
        for layer_path in services.layer_stack.iter_paths(lease.manifest):
            if is_vcs_excluded(layer_path):
                continue
            if not under(sub_path, layer_path):
                continue
            if not fnmatch.fnmatchcase(layer_path, pattern):
                continue
            matches.append(layer_path)
        iter_elapsed = monotonic_now() - iter_start
        matches.sort()
        truncated = len(matches) > DEFAULT_GLOB_LIMIT
        return {
            "success": True,
            "filenames": matches[:DEFAULT_GLOB_LIMIT],
            "num_files": min(len(matches), DEFAULT_GLOB_LIMIT),
            "truncated": truncated,
            "timings": {
                "api.glob.lease_acquire_s": lease_acquired_s,
                "api.glob.iter_s": iter_elapsed,
                "api.glob.total_s": monotonic_now() - total_start,
            },
        }
    finally:
        services.manager.release_lease(lease.lease_id)


async def glob(args: dict[str, Any]) -> dict[str, Any]:
    """``api.glob``: enumerate snapshot paths matching a glob pattern."""
    return await run_sync_in_executor(_glob_sync, args)


__all__ = ["DEFAULT_GLOB_LIMIT", "glob"]
