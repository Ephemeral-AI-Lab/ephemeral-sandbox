"""Overlay lifecycle primitives shared by workspace pipelines."""

from __future__ import annotations

import shutil
from collections.abc import Sequence
from uuid import uuid4

from sandbox._shared.shell_contract import WorkspaceLeaseClient
from sandbox.overlay.capture import walk_upperdir
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.path_change import OverlayPathChange
from sandbox.overlay.writable_dirs import (
    allocate_overlay_writable_dirs,
    overlay_writable_root,
)


async def create(
    layer_stack: WorkspaceLeaseClient,
    *,
    agent_id: str,
    workspace_root: str = "/testbed",
) -> OverlayHandle:
    """Lease a snapshot and allocate upper/work dirs for a workspace overlay."""
    invocation_id = f"overlay:{agent_id}:{uuid4().hex[:8]}"
    run_dir = (
        overlay_writable_root()
        / "runtime"
        / "overlay"
        / invocation_id.replace(":", "-")
    )
    writable_dirs = allocate_overlay_writable_dirs(run_dir)
    lease = layer_stack.prepare_workspace_snapshot(request_id=invocation_id)
    if lease.layer_paths is None:
        layer_stack.release_lease(lease_id=lease.lease_id)
        shutil.rmtree(run_dir, ignore_errors=True)
        raise RuntimeError("overlay lifecycle requires namespace layer paths")
    return OverlayHandle(
        workspace_root=workspace_root,
        layer_paths=tuple(lease.layer_paths),
        upperdir=writable_dirs.upperdir,
        workdir=writable_dirs.workdir,
        snapshot_version=lease.manifest_version,
        lease_id=lease.lease_id,
        namespace_pid=None,
        snapshot_manifest=getattr(lease, "manifest", None),
        _release=lambda: layer_stack.release_lease(lease_id=lease.lease_id),
    )


async def capture_changes(handle: OverlayHandle) -> Sequence[OverlayPathChange]:
    return walk_upperdir(handle.upperdir)


async def destroy(handle: OverlayHandle) -> None:
    """Idempotently mark an overlay handle destroyed and clean upper/work dirs."""
    with handle._destroy_lock:
        if handle._destroyed:
            return
        handle._destroyed = True
        if handle._release is not None:
            handle._release()
        shutil.rmtree(handle.upperdir.parent, ignore_errors=True)


__all__ = ["capture_changes", "create", "destroy"]
