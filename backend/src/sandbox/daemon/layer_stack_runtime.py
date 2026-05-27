"""Daemon-local layer-stack runtime cache and base construction."""

from __future__ import annotations

import shutil
import threading
import time
from pathlib import Path

from sandbox.daemon.audit_schema import (
    LayerStackSection,
    build_layer_stack_event,
    safe_emit,
)
from sandbox.layer_stack.stack import (
    LayerStackSnapshotLease,
    LayerStack,
)
from sandbox.layer_stack.manifest import manifest_path, read_manifest
from sandbox.layer_stack.workspace_base import (
    build_workspace_base as _build_layer_stack_workspace_base,
)
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBinding,
    WorkspaceBindingError,
    read_workspace_binding,
    require_workspace_binding,
)
from sandbox.shared.clock import monotonic_now

_MANAGER_CACHE_LOCK = threading.RLock()
_MANAGER_CACHE: dict[str, LayerStack] = {}
_DAEMON_STARTED_AT_WALLCLOCK = time.time()
_FENCED_STAGING_ROOTS: set[str] = set()


def get_layer_stack_manager(layer_stack_root: str | Path) -> LayerStack:
    key = str(Path(layer_stack_root).resolve(strict=False))
    with _MANAGER_CACHE_LOCK:
        _fence_stale_staging_once(key)
        manager = _MANAGER_CACHE.get(key)
        if manager is None:
            manager = LayerStack(key)
            _MANAGER_CACHE[key] = manager
        return manager


def drop_layer_stack_manager(layer_stack_root: str | Path) -> None:
    key = str(Path(layer_stack_root).resolve(strict=False))
    with _MANAGER_CACHE_LOCK:
        _MANAGER_CACHE.pop(key, None)
        _FENCED_STAGING_ROOTS.discard(key)


def fence_stale_staging(layer_stack_root: str | Path) -> dict[str, object]:
    """Remove staging dirs that predate the current daemon process."""
    total_start = monotonic_now()
    staging_root = Path(layer_stack_root).resolve(strict=False) / "staging"
    inspected_dirs = 0
    fenced_paths: list[str] = []
    if staging_root.is_dir():
        for child in sorted(staging_root.iterdir(), key=lambda path: path.name):
            if child.is_symlink() or not child.is_dir():
                continue
            inspected_dirs += 1
            try:
                mtime = child.lstat().st_mtime
            except OSError:
                continue
            if mtime >= _DAEMON_STARTED_AT_WALLCLOCK:
                continue
            shutil.rmtree(child, ignore_errors=True)
            if not child.exists():
                fenced_paths.append(child.as_posix())
    return {
        "success": True,
        "staging_root": staging_root.as_posix(),
        "inspected_dirs": inspected_dirs,
        "fenced_dirs": len(fenced_paths),
        "fenced_paths": fenced_paths,
        "timings": {
            "layer_stack.fence_stale_staging_s": monotonic_now() - total_start,
        },
    }


def _fence_stale_staging_once(layer_stack_root: str) -> None:
    if layer_stack_root in _FENCED_STAGING_ROOTS:
        return
    fence_stale_staging(layer_stack_root)
    _FENCED_STAGING_ROOTS.add(layer_stack_root)


def clear_layer_stack_runtime_caches_for_tests() -> None:
    with _MANAGER_CACHE_LOCK:
        _MANAGER_CACHE.clear()
        _FENCED_STAGING_ROOTS.clear()
        _LEASE_REQUEST_TIMESTAMPS.clear()


def build_workspace_base(
    layer_stack_root: str | Path,
    *,
    workspace_root: str | Path,
    reset: bool = False,
    timings: dict[str, float] | None = None,
) -> WorkspaceBinding:
    """Build (or rebuild on ``reset``) the workspace base for one root."""
    if reset:
        drop_layer_stack_manager(layer_stack_root)
    return _build_layer_stack_workspace_base(
        workspace_root=workspace_root,
        layer_stack_root=layer_stack_root,
        reset=reset,
        timings=timings,
    )


def ensure_workspace_base(
    layer_stack_root: str | Path,
    *,
    workspace_root: str | Path,
) -> tuple[WorkspaceBinding, bool]:
    """Return the existing binding for ``layer_stack_root`` or build a new base."""
    binding = read_workspace_binding(layer_stack_root)
    if binding is not None:
        _validate_manifest_for_root(Path(layer_stack_root))
        if Path(binding.workspace_root) != Path(workspace_root):
            raise WorkspaceBindingError(
                "workspace binding points at a different workspace: "
                f"{binding.workspace_root} != {workspace_root}"
            )
        return binding, False
    return build_workspace_base(layer_stack_root, workspace_root=workspace_root), True


def acquire_snapshot(
    layer_stack_root: str | Path,
    *,
    owner_request_id: str,
) -> LayerStackSnapshotLease:
    """Prepare a workspace snapshot lease for a bound, manifest-valid root."""
    require_workspace_binding(layer_stack_root)
    _validate_manifest_for_root(Path(layer_stack_root))
    started = monotonic_now()
    _emit_layer_stack_event(
        "layer_stack.lease_requested",
        LayerStackSection(
            operation_id=owner_request_id,
            operation_step=20,
            owner_request_id=owner_request_id,
        ),
        lane="normal",
    )
    result = get_layer_stack_manager(layer_stack_root).acquire_snapshot(
        owner_request_id,
    )
    elapsed_ms = (monotonic_now() - started) * 1000.0
    _emit_layer_stack_event(
        "layer_stack.lease_acquired",
        LayerStackSection(
            operation_id=owner_request_id,
            operation_step=20,
            owner_request_id=owner_request_id,
            lease_id=result.lease_id,
            manifest_version=result.manifest_version,
            manifest_root_hash=result.root_hash,
            lease_wait_ms=elapsed_ms,
        ),
        lane="normal",
    )
    _emit_layer_stack_event(
        "layer_stack.snapshot_prepared",
        LayerStackSection(
            operation_id=owner_request_id,
            operation_step=40,
            lease_id=result.lease_id,
            manifest_version=result.manifest_version,
            manifest_root_hash=result.root_hash,
            layer_count=len(result.layer_paths),
            prepare_snapshot_ms=elapsed_ms,
        ),
        lane="normal",
    )
    _LEASE_REQUEST_TIMESTAMPS[result.lease_id] = (owner_request_id, monotonic_now())
    return result


def commit_to_workspace(
    layer_stack_root: str | Path,
    *,
    workspace_root: str | Path,
    timings: dict[str, float] | None = None,
):
    """Project the active overlay onto ``workspace_root`` and rebuild the base.

    Refuses to run while any snapshot lease is active. After the call,
    layer storage is reset and the workspace becomes the new base.
    """
    drop_layer_stack_manager(layer_stack_root)
    new_manifest = get_layer_stack_manager(layer_stack_root).commit_to_workspace(
        workspace_root=workspace_root,
        timings=timings,
    )
    drop_layer_stack_manager(layer_stack_root)
    return new_manifest


def release_lease(
    layer_stack_root: str | Path,
    *,
    lease_id: str,
) -> bool:
    """Release a previously-prepared workspace snapshot lease."""
    released = get_layer_stack_manager(layer_stack_root).release_lease(lease_id)
    if released:
        operation_id, started = _LEASE_REQUEST_TIMESTAMPS.pop(
            lease_id, (None, monotonic_now())
        )
        _emit_layer_stack_event(
            "layer_stack.lease_released",
            LayerStackSection(
                operation_id=operation_id,
                operation_step=130,
                lease_id=lease_id,
                lease_hold_ms=(monotonic_now() - started) * 1000.0,
            ),
            lane="normal",
        )
    return released


_LEASE_REQUEST_TIMESTAMPS: dict[str, tuple[str | None, float]] = {}


def _emit_layer_stack_event(
    event_type: str,
    section: LayerStackSection,
    *,
    lane: str,
) -> None:
    safe_emit(
        build_layer_stack_event(event_type, section),
        lane=lane,  # type: ignore[arg-type]
    )


def emit_squash_event(
    *,
    triggered: bool = False,
    completed: bool = False,
    failed: bool = False,
    trigger_reason: str | None = None,
    input_layers: int | None = None,
    result_layers: int | None = None,
    manifest_root_hash_value: str | None = None,
    failure_kind: str | None = None,
) -> None:
    """Emit one of the ``layer_stack.squash_*`` critical-lane events.

    Callers in the squash plan path invoke this so the audit ring sees a
    contiguous ``triggered → {completed | failed}`` pair.
    """
    if triggered:
        _emit_layer_stack_event(
            "layer_stack.squash_triggered",
            LayerStackSection(
                squash_trigger_reason=trigger_reason,
                squash_input_layers=input_layers,
            ),
            lane="critical",
        )
    if completed:
        _emit_layer_stack_event(
            "layer_stack.squash_completed",
            LayerStackSection(
                squash_input_layers=input_layers,
                squash_result_layers=result_layers,
                manifest_root_hash=manifest_root_hash_value,
            ),
            lane="critical",
        )
    if failed:
        _emit_layer_stack_event(
            "layer_stack.squash_failed",
            LayerStackSection(
                squash_failure_kind=failure_kind,
                manifest_root_hash=manifest_root_hash_value,
            ),
            lane="critical",
        )


def _validate_manifest_for_root(layer_stack_root: Path) -> None:
    manifest_file = manifest_path(layer_stack_root)
    if not manifest_file.exists():
        raise WorkspaceBindingError(
            f"active manifest is missing for workspace binding: {manifest_file}"
        )
    active = read_manifest(manifest_file)
    if active.version <= 0:
        raise WorkspaceBindingError(
            f"active manifest is empty for workspace binding: {manifest_file}"
        )


__all__ = [
    "build_workspace_base",
    "clear_layer_stack_runtime_caches_for_tests",
    "commit_to_workspace",
    "drop_layer_stack_manager",
    "emit_squash_event",
    "ensure_workspace_base",
    "fence_stale_staging",
    "get_layer_stack_manager",
    "acquire_snapshot",
    "release_lease",
]
