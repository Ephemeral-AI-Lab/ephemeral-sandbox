"""``api.layer_metrics`` diagnostic dispatch entry."""

from __future__ import annotations

from collections.abc import Mapping

from sandbox.layer_stack import LayerStackManager
from sandbox.layer_stack.workspace.binding import read_workspace_binding
from sandbox.runtime.daemon.service import occ_backend


async def layer_metrics(args: dict[str, object]) -> dict[str, object]:
    """Summarize layer-stack storage and lease state for one runtime root."""
    layer_stack_root = str(args.get("layer_stack_root") or "")
    manager = _manager(args)
    manifest = manager.read_active_manifest()
    binding = read_workspace_binding(layer_stack_root)
    backend = occ_backend.build_occ_backend(layer_stack_root)
    layer_dirs = tuple((manager.storage_root / "layers").iterdir())
    staging_dirs = tuple((manager.storage_root / "staging").iterdir())
    total_bytes = 0
    for entry in manager.storage_root.rglob("*"):
        if entry.is_file() or entry.is_symlink():
            total_bytes += entry.lstat().st_size
    return {
        "success": True,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth,
        "active_leases": manager.active_lease_count(),
        "pinned_layers": len(manager.pinned_layers()),
        "layer_dirs": len(layer_dirs),
        "staging_dirs": len(staging_dirs),
        "storage_bytes": total_bytes,
        "workspace_bound": binding is not None,
        "workspace_root": binding.workspace_root if binding is not None else "",
        "base_root_hash": binding.base_root_hash if binding is not None else "",
        "auto_squash": backend.occ_service.auto_squash_maintenance_status(),
    }


def _manager(args: Mapping[str, object]) -> LayerStackManager:
    layer_stack_root = str(args.get("layer_stack_root") or "").strip()
    if not layer_stack_root:
        raise ValueError("layer_stack_root is required")
    return occ_backend.build_occ_backend(layer_stack_root).manager


__all__ = ["layer_metrics"]
