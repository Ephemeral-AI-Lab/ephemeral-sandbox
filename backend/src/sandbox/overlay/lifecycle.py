"""Overlay lifecycle primitives shared by workspace pipelines."""

from __future__ import annotations

import shutil
from collections.abc import Callable, Sequence
from pathlib import Path
from uuid import uuid4

from sandbox._shared.clock import monotonic_now
from sandbox._shared.layer_stack_port import LayerStackSnapshotPort
from sandbox.audit.schema import (
    OverlayWorkspaceSection,
    build_overlay_workspace_event,
    safe_emit,
    safe_record_phase,
)
from sandbox.overlay.capture import walk_upperdir
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.path_change import OverlayPathChange
from sandbox.overlay.writable_dirs import (
    allocate_overlay_writable_dirs,
    overlay_writable_root,
)


def acquire(
    layer_stack: LayerStackSnapshotPort,
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

    On any exception after ``acquire_snapshot`` succeeds, this
    function releases the lease AND ``rmtree(run_dir)`` before re-raising so
    no lease or scratch directory leaks past the error boundary.
    """
    run_dir = _allocate_run_dir(invocation_id)
    mount_started = monotonic_now()
    snapshot = layer_stack.acquire_snapshot(request_id=invocation_id)
    lease_id = str(getattr(snapshot, "lease_id"))
    try:
        layer_paths = getattr(snapshot, "layer_paths", None)
        if layer_paths is None:
            raise RuntimeError("overlay snapshot did not provide layer paths")
        writable_dirs = allocate_overlay_writable_dirs(run_dir)
        manifest = getattr(snapshot, "manifest", None)
        manifest_version = int(getattr(snapshot, "manifest_version", 0))
        root_hash = str(getattr(snapshot, "root_hash", "") or "")
        mount_ms = (monotonic_now() - mount_started) * 1000.0
        safe_emit(
            build_overlay_workspace_event(
                "overlay_workspace.mounted",
                OverlayWorkspaceSection(
                    operation_id=invocation_id,
                    workspace_handle_id=lease_id,
                    lease_id=lease_id,
                    manifest_root_hash=root_hash or None,
                    mount_ms=mount_ms,
                ),
            ),
            lane="critical",
        )
        # V3 §2/§3 — surface the mount phase in the per-tool rollup so
        # `phase_totals_rollup.mount_ms` is populated for any tool call
        # whose framework dispatcher set up an active phase buffer.
        safe_record_phase("mount", mount_ms)
        return OverlayHandle(
            workspace_root=str(workspace_root).rstrip("/") or "/",
            layer_paths=tuple(str(path) for path in layer_paths),
            upperdir=writable_dirs.upperdir,
            workdir=writable_dirs.workdir,
            lease_id=lease_id,
            holder_pid=None,
            run_dir=run_dir,
            snapshot_manifest=manifest,
            snapshot_timings=dict(getattr(snapshot, "timings", {}) or {}),
            manifest_key=f"{root_hash}@{manifest_version}",
            manifest_version=manifest_version,
            root_hash=root_hash,
            operation_id=invocation_id,
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


async def release_overlay(handle: OverlayHandle) -> None:
    """Idempotently release an overlay handle and clean upper/work dirs."""
    handle.release()
    cleanup_started = monotonic_now()
    cleanup_errors: list[OSError] = []

    def _onerror(_func, _path, exc_info) -> None:
        exc = exc_info[1] if exc_info else None
        if isinstance(exc, FileNotFoundError):
            return  # release closure may have already removed run_dir
        if isinstance(exc, OSError):
            cleanup_errors.append(exc)

    if handle.run_dir.exists():
        shutil.rmtree(handle.run_dir, onerror=_onerror)
    elapsed_ms = (monotonic_now() - cleanup_started) * 1000.0
    scratch_removed = not handle.run_dir.exists()
    if cleanup_errors or not scratch_removed:
        kind = (
            type(cleanup_errors[0]).__name__
            if cleanup_errors
            else "scratch_path_persisted"
        )
        _emit_overlay_workspace_cleanup_failed(
            handle, cleanup_failure_kind=kind, cleanup_ms=elapsed_ms
        )
    else:
        _emit_overlay_workspace_cleaned(handle, cleanup_ms=elapsed_ms)


def _emit_overlay_workspace_cleaned(
    handle: OverlayHandle, *, cleanup_ms: float
) -> None:
    _emit_overlay_workspace_cleanup_event(
        "overlay_workspace.cleaned",
        handle,
        cleanup_ms=cleanup_ms,
        scratch_removed=True,
    )


def _emit_overlay_workspace_cleanup_failed(
    handle: OverlayHandle,
    *,
    cleanup_failure_kind: str,
    cleanup_ms: float,
) -> None:
    _emit_overlay_workspace_cleanup_event(
        "overlay_workspace.cleanup_failed",
        handle,
        cleanup_failure_kind=cleanup_failure_kind,
        cleanup_ms=cleanup_ms,
        scratch_removed=False,
    )


def _emit_overlay_workspace_cleanup_event(
    event_type: str,
    handle: OverlayHandle,
    *,
    cleanup_ms: float,
    scratch_removed: bool,
    cleanup_failure_kind: str | None = None,
) -> None:
    safe_emit(
        build_overlay_workspace_event(
            event_type,
            OverlayWorkspaceSection(
                operation_id=handle.operation_id or None,
                workspace_handle_id=handle.lease_id or None,
                lease_id=handle.lease_id or None,
                cleanup_failure_kind=cleanup_failure_kind,
                cleanup_ms=cleanup_ms,
                scratch_removed=scratch_removed,
            ),
        ),
        lane="critical",
    )


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
    layer_stack: LayerStackSnapshotPort,
    lease_id: str,
    run_dir: Path,
    release_hook: Callable[[str], None] | None,
) -> Callable[[], None]:
    # ``run_dir`` is rmtree'd by ``release_overlay`` itself (which owns the
    # cleanup-event emission); the closure here only releases the lease so
    # that we have a single owner for scratch cleanup.
    del run_dir

    def _release() -> None:
        _release_lease(layer_stack, lease_id, release_hook=release_hook)

    return _release


def _release_lease(
    layer_stack: LayerStackSnapshotPort,
    lease_id: str,
    *,
    release_hook: Callable[[str], None] | None,
) -> None:
    if release_hook is not None:
        release_hook(lease_id)
    else:
        layer_stack.release_lease(lease_id=lease_id)


def _release_lease_silently(
    layer_stack: LayerStackSnapshotPort,
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


__all__ = [
    "acquire",
    "capture_changes",
    "release_overlay",
]
