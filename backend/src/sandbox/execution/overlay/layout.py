"""Shared layout for command-exec workspace replacement strategies."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from sandbox.execution.overlay.new_mount_api import (
    OVL_MAX_STACK_GUARD,
    LayerStackTooDeep,
)


@dataclass
class MaterializeLayout:
    """Layout used by materialize=True callers (plugin, copy-backed, debug)."""

    workspace_root: str
    base_repo: str
    writes: str
    kernel_scratch: str
    scratch_root: str

    def __post_init__(self) -> None:
        if not str(self.workspace_root).startswith("/"):
            raise ValueError("workspace_root must be absolute")
        if not str(self.scratch_root).strip():
            raise ValueError("scratch_root must not be empty")
        scratch_root = Path(self.scratch_root).resolve(strict=False)
        resolved_paths: dict[str, Path] = {}
        for field_name in ("base_repo", "writes", "kernel_scratch"):
            if not str(getattr(self, field_name)).strip():
                raise ValueError(f"{field_name} must not be empty")
            path = Path(str(getattr(self, field_name))).resolve(strict=False)
            if path == scratch_root or not path.is_relative_to(scratch_root):
                raise ValueError(
                    f"{field_name} must be strictly under scratch_root: {path}"
                )
            resolved_paths[field_name] = path

        seen: dict[Path, str] = {}
        for field_name, path in resolved_paths.items():
            duplicate = seen.get(path)
            if duplicate is not None:
                raise ValueError(
                    f"{field_name} must be distinct from {duplicate}: {path}"
                )
            seen[path] = field_name


@dataclass
class LayerPathsLayout:
    """Layout used by materialize=False callers (namespace strategy)."""

    workspace_root: str
    layer_paths: tuple[str, ...]
    layer_storage_root: str
    writes: str
    kernel_scratch: str
    scratch_root: str

    def __post_init__(self) -> None:
        if not str(self.workspace_root).startswith("/"):
            raise ValueError("workspace_root must be absolute")
        if not str(self.scratch_root).strip():
            raise ValueError("scratch_root must not be empty")
        if not self.layer_paths:
            raise ValueError("layer_paths must not be empty")
        if not str(self.layer_storage_root).strip():
            raise ValueError("layer_storage_root must not be empty")
        if len(self.layer_paths) > OVL_MAX_STACK_GUARD:
            raise LayerStackTooDeep(
                f"manifest depth {len(self.layer_paths)} exceeds "
                f"OVL_MAX_STACK_GUARD={OVL_MAX_STACK_GUARD}"
            )
        layer_storage_root = Path(self.layer_storage_root).resolve(strict=False)
        for path_str in self.layer_paths:
            path = Path(path_str).resolve(strict=False)
            if path == layer_storage_root or not path.is_relative_to(layer_storage_root):
                raise ValueError(
                    f"layer path {path_str!r} must be under "
                    f"layer_storage_root {self.layer_storage_root!r}"
                )
        scratch_root = Path(self.scratch_root).resolve(strict=False)
        for field_name in ("writes", "kernel_scratch"):
            if not str(getattr(self, field_name)).strip():
                raise ValueError(f"{field_name} must not be empty")
            path = Path(str(getattr(self, field_name))).resolve(strict=False)
            if path == scratch_root or not path.is_relative_to(scratch_root):
                raise ValueError(
                    f"{field_name} must be strictly under scratch_root: {path}"
                )


# Starter alias so existing callers of OverlayLayout(...) continue to work
# during the migration. T4/T5 will narrow call sites to MaterializeLayout or
# LayerPathsLayout explicitly, at which point this alias becomes the union.
OverlayLayout = MaterializeLayout

# Union type for use in type annotations (isinstance dispatch in T4/T5).
AnyOverlayLayout = MaterializeLayout | LayerPathsLayout


__all__ = ["MaterializeLayout", "LayerPathsLayout", "OverlayLayout", "AnyOverlayLayout"]
