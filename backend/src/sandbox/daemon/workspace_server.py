"""Runtime-local workspace server for layer-stack base construction."""

from __future__ import annotations

import shutil
import threading
import time
from pathlib import Path

from sandbox.layer_stack.stack import (
    LayerStack,
    PrepareWorkspaceSnapshotResult,
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
from sandbox._shared.clock import monotonic_now

_MANAGER_CACHE_LOCK = threading.RLock()
_MANAGER_CACHE: dict[str, LayerStack] = {}
_DAEMON_STARTED_AT = time.time()
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
            if mtime >= _DAEMON_STARTED_AT:
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


def clear_layer_stack_server_caches_for_tests() -> None:
    with _MANAGER_CACHE_LOCK:
        _MANAGER_CACHE.clear()
        _FENCED_STAGING_ROOTS.clear()


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


def prepare_workspace_snapshot(
    layer_stack_root: str | Path,
    *,
    owner_request_id: str,
) -> PrepareWorkspaceSnapshotResult:
    """Prepare a workspace snapshot lease for a bound, manifest-valid root."""
    require_workspace_binding(layer_stack_root)
    _validate_manifest_for_root(Path(layer_stack_root))
    return get_layer_stack_manager(layer_stack_root).prepare_workspace_snapshot(
        owner_request_id,
    )


def release_workspace_snapshot(
    layer_stack_root: str | Path,
    *,
    lease_id: str,
) -> bool:
    """Release a previously-prepared workspace snapshot lease."""
    return get_layer_stack_manager(layer_stack_root).release_lease(lease_id)


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
    "clear_layer_stack_server_caches_for_tests",
    "drop_layer_stack_manager",
    "ensure_workspace_base",
    "fence_stale_staging",
    "get_layer_stack_manager",
    "prepare_workspace_snapshot",
    "release_workspace_snapshot",
]
