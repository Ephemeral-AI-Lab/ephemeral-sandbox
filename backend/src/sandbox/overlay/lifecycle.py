"""Overlay lifecycle primitives shared by workspace pipelines."""

from __future__ import annotations

import shutil
from collections.abc import Callable, Sequence
from pathlib import Path
from uuid import uuid4

from sandbox._shared.layer_stack_port import LayerStackPort
from sandbox.overlay.capture import walk_upperdir
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.path_change import OverlayPathChange
from sandbox.overlay.writable_dirs import (
    allocate_overlay_writable_dirs,
    overlay_writable_root,
)


def acquire(
    layer_stack: LayerStackPort,
    *,
    invocation_id: str,
    workspace_root: str = "/testbed",
    release_hook: Callable[[str], None] | None = None,
) -> OverlayHandle:
    """Lease a snapshot, allocate upper/work dirs, and assemble an ``OverlayHandle``.

    The sole "lease + writable_dirs + error-cleanup" primitive. Callers pick
    the release strategy via ``release_hook``:

    * ``release_hook=None`` (default) binds the handle's release to
      ``layer_stack.release_lease(lease_id=...)``. Suitable for projection
      callers that do not need ``LeaseGuard``/audit routing.
    * Operation-overlay callers pass their own release function when handles
      can be released directly instead of through a pipeline destroy guard.

    On any exception after ``prepare_workspace_snapshot`` succeeds, this
    function releases the lease AND ``rmtree(run_dir)`` before re-raising so
    no lease or scratch directory leaks past the error boundary.
    """
    run_dir = _allocate_run_dir(invocation_id)
    snapshot = layer_stack.prepare_workspace_snapshot(request_id=invocation_id)
    lease_id = str(getattr(snapshot, "lease_id"))
    try:
        layer_paths = getattr(snapshot, "layer_paths", None)
        if layer_paths is None:
            raise RuntimeError("overlay snapshot did not provide layer paths")
        writable_dirs = allocate_overlay_writable_dirs(run_dir)
        manifest = getattr(snapshot, "manifest", None)
        manifest_version = int(getattr(snapshot, "manifest_version", 0))
        root_hash = str(getattr(snapshot, "root_hash", "") or "")
        return OverlayHandle(
            workspace_root=str(workspace_root).rstrip("/") or "/",
            layer_paths=tuple(str(path) for path in layer_paths),
            upperdir=writable_dirs.upperdir,
            workdir=writable_dirs.workdir,
            snapshot_version=manifest_version,
            lease_id=lease_id,
            namespace_pid=None,
            run_dir=run_dir,
            snapshot_manifest=manifest,
            snapshot_timings=dict(getattr(snapshot, "timings", {}) or {}),
            manifest_key=f"{root_hash}@{manifest_version}",
            manifest_version=manifest_version,
            root_hash=root_hash,
            _release=_build_release_closure(
                layer_stack=layer_stack,
                lease_id=lease_id,
                run_dir=run_dir,
                release_hook=release_hook,
            ),
        )
    except Exception:
        _release_lease_silently(layer_stack, lease_id, release_hook=release_hook)
        shutil.rmtree(run_dir, ignore_errors=True)
        raise


async def capture_changes(handle: OverlayHandle) -> Sequence[OverlayPathChange]:
    return walk_upperdir(handle.upperdir)


async def destroy(handle: OverlayHandle) -> None:
    """Idempotently mark an overlay handle destroyed and clean upper/work dirs."""
    handle.release()
    shutil.rmtree(handle.run_dir, ignore_errors=True)


def _allocate_run_dir(invocation_id: str) -> Path:
    safe = _safe_invocation_part(invocation_id)
    return overlay_writable_root() / "runtime" / "overlay" / f"{safe}-{uuid4().hex[:8]}"


def _safe_invocation_part(value: str) -> str:
    safe = "".join(
        char if char.isalnum() or char in ("-", "_") else "-" for char in str(value)
    ).strip("-")
    return safe or "overlay"


def _build_release_closure(
    *,
    layer_stack: LayerStackPort,
    lease_id: str,
    run_dir: Path,
    release_hook: Callable[[str], None] | None,
) -> Callable[[], None]:
    def _release() -> None:
        try:
            _release_lease(layer_stack, lease_id, release_hook=release_hook)
        finally:
            shutil.rmtree(run_dir, ignore_errors=True)

    return _release


def _release_lease(
    layer_stack: LayerStackPort,
    lease_id: str,
    *,
    release_hook: Callable[[str], None] | None,
) -> None:
    if release_hook is not None:
        release_hook(lease_id)
    else:
        layer_stack.release_lease(lease_id=lease_id)


def _release_lease_silently(
    layer_stack: LayerStackPort,
    lease_id: str,
    *,
    release_hook: Callable[[str], None] | None,
) -> None:
    if not lease_id:
        return
    try:
        _release_lease(layer_stack, lease_id, release_hook=release_hook)
    except Exception:
        pass


__all__ = ["acquire", "capture_changes", "destroy"]
