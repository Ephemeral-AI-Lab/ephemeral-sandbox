"""Checkpoint-based depth control for sandbox layer stacks."""

from __future__ import annotations

import os
import shutil
import uuid
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack._paths import resolve_storage_path
from sandbox.layer_stack.manifest import LAYERS_DIR, STAGING_DIR, LayerRef, Manifest
from sandbox.layer_stack.view.merged import MergedView


@dataclass(frozen=True)
class SquashPlan:
    active_version: int
    suffix_to_checkpoint: tuple[LayerRef, ...]

    def __post_init__(self) -> None:
        object.__setattr__(
            self,
            "suffix_to_checkpoint",
            tuple(self.suffix_to_checkpoint),
        )
        if not self.suffix_to_checkpoint:
            raise ValueError("suffix_to_checkpoint must not be empty")


class SquashWorker:
    """Plans suffix squash and materializes checkpoint layers."""

    def __init__(
        self,
        storage_root: str | Path,
    ) -> None:
        self._storage_root = Path(storage_root)
        self._view = MergedView(self._storage_root)

    def plan(self, active_manifest: Manifest, *, max_depth: int) -> SquashPlan | None:
        if max_depth <= 0:
            raise ValueError("max_depth must be positive")
        if active_manifest.depth <= max_depth:
            return None

        suffix_depth = active_manifest.depth - max_depth + 1
        if suffix_depth <= 1:
            return None

        return SquashPlan(
            active_version=active_manifest.version,
            suffix_to_checkpoint=active_manifest.layers[-suffix_depth:],
        )

    def build_checkpoint(self, plan: SquashPlan) -> LayerRef:
        layer_id, staging_dir, layer_dir = self._allocate_checkpoint_paths(plan.active_version + 1)
        suffix_manifest = Manifest(
            version=plan.active_version,
            layers=plan.suffix_to_checkpoint,
        )
        try:
            self._view.materialize(staging_dir, suffix_manifest)
            layer_dir.parent.mkdir(parents=True, exist_ok=True)
            os.replace(staging_dir, layer_dir)
        except Exception:
            shutil.rmtree(staging_dir, ignore_errors=True)
            raise
        return LayerRef(layer_id=layer_id, path=f"{LAYERS_DIR}/{layer_id}")

    def relabel_checkpoint(self, checkpoint: LayerRef, *, manifest_version: int) -> LayerRef:
        """Rename a prebuilt checkpoint to match the manifest that will publish it."""
        current_path = resolve_storage_path(self._storage_root, checkpoint.path)
        if not current_path.exists():
            raise FileNotFoundError(f"checkpoint layer is missing: {checkpoint.layer_id}")

        layer_id, _staging_dir, layer_dir = self._allocate_checkpoint_paths(
            manifest_version
        )
        os.replace(current_path, layer_dir)
        return LayerRef(layer_id=layer_id, path=f"{LAYERS_DIR}/{layer_id}")

    def discard_checkpoint(self, checkpoint: LayerRef) -> None:
        layer_path = resolve_storage_path(self._storage_root, checkpoint.path)
        shutil.rmtree(layer_path, ignore_errors=True)

    def _allocate_checkpoint_paths(self, next_version: int) -> tuple[str, Path, Path]:
        for _ in range(100):
            layer_id = _default_checkpoint_id(next_version)
            layer_dir = self._storage_root / LAYERS_DIR / layer_id
            staging_dir = self._storage_root / STAGING_DIR / f"{layer_id}.staging"
            if not layer_dir.exists() and not staging_dir.exists():
                return layer_id, staging_dir, layer_dir
        raise RuntimeError("could not allocate a unique checkpoint layer id")


def manifest_still_ends_with(
    manifest: Manifest,
    suffix: tuple[LayerRef, ...],
) -> bool:
    if len(suffix) > len(manifest.layers):
        return False
    return manifest.layers[-len(suffix) :] == suffix


def _default_checkpoint_id(next_version: int) -> str:
    return f"B{next_version:06d}-{uuid.uuid4().hex[:8]}"
