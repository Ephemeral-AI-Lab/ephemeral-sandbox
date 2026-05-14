"""Runtime-local workspace server for layer-stack base construction."""

from __future__ import annotations

import shutil
import threading
import time
from pathlib import Path

from sandbox.layer_stack.manager import (
    LayerStackManager,
    PrepareWorkspaceSnapshotResult,
)
from sandbox.layer_stack.manifest import manifest_path, read_manifest
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBinding,
    WorkspaceBindingError,
    read_workspace_binding,
    require_workspace_binding,
)
from sandbox.timing import monotonic_now

_MANAGER_CACHE_LOCK = threading.RLock()
_MANAGER_CACHE: dict[str, LayerStackManager] = {}
_DAEMON_STARTED_AT = time.time()
_FENCED_STAGING_ROOTS: set[str] = set()


def get_layer_stack_manager(layer_stack_root: str | Path) -> LayerStackManager:
    key = str(Path(layer_stack_root).resolve(strict=False))
    with _MANAGER_CACHE_LOCK:
        _fence_stale_staging_once(key)
        manager = _MANAGER_CACHE.get(key)
        if manager is None:
            manager = LayerStackManager(key)
            _MANAGER_CACHE[key] = manager
        return manager


def drop_layer_stack_manager(layer_stack_root: str | Path) -> None:
    key = str(Path(layer_stack_root).resolve(strict=False))
    with _MANAGER_CACHE_LOCK:
        _MANAGER_CACHE.pop(key, None)


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
                mtime = child.stat().st_mtime
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


class LayerStackWorkspaceServer:
    """Owns binding and first base build for one layer-stack root."""

    def __init__(self, layer_stack_root: str | Path) -> None:
        self.layer_stack_root = Path(layer_stack_root)
        self._manager = get_layer_stack_manager(self.layer_stack_root)

    def build_workspace_base(
        self,
        *,
        workspace_root: str | Path,
        reset: bool = False,
        timings: dict[str, float] | None = None,
    ) -> WorkspaceBinding:
        if reset:
            drop_layer_stack_manager(self.layer_stack_root)
        binding = build_workspace_base(
            workspace_root=workspace_root,
            layer_stack_root=self.layer_stack_root,
            reset=reset,
            timings=timings,
        )
        self._manager = get_layer_stack_manager(self.layer_stack_root)
        return binding

    def ensure_workspace_base(
        self,
        *,
        workspace_root: str | Path,
    ) -> tuple[WorkspaceBinding, bool]:
        binding = read_workspace_binding(self.layer_stack_root)
        if binding is not None:
            _validate_manifest_for_root(self.layer_stack_root)
            if Path(binding.workspace_root) != Path(workspace_root):
                raise WorkspaceBindingError(
                    "workspace binding points at a different workspace: "
                    f"{binding.workspace_root} != {workspace_root}"
                )
            return binding, False
        return self.build_workspace_base(
            workspace_root=workspace_root,
        ), True

    def prepare_workspace_snapshot(
        self,
        *,
        owner_request_id: str,
    ) -> PrepareWorkspaceSnapshotResult:
        self._require_bound_active_workspace()
        return self._manager.prepare_workspace_snapshot(
            owner_request_id,
        )

    def release_workspace_snapshot(self, *, lease_id: str) -> bool:
        return self._manager.release_lease(lease_id)

    def _require_bound_active_workspace(self) -> WorkspaceBinding:
        binding = require_workspace_binding(self.layer_stack_root)
        _validate_manifest_for_root(self.layer_stack_root)
        return binding


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
    "LayerStackWorkspaceServer",
    "clear_layer_stack_server_caches_for_tests",
    "drop_layer_stack_manager",
    "fence_stale_staging",
    "get_layer_stack_manager",
]
