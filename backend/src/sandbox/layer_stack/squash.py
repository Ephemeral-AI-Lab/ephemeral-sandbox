"""Checkpoint-based depth control for sandbox layer stacks."""

from __future__ import annotations

import os
import shutil
import uuid
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.paths import (
    allocate_unique_layer_paths,
    fsync_path,
    resolve_storage_path,
)
from sandbox.layer_stack.manifest import LAYERS_DIR, STAGING_DIR, LayerRef, Manifest
from sandbox.layer_stack.view import MergedView


@dataclass(frozen=True)
class CheckpointSegment:
    layers: tuple[LayerRef, ...]

    def __post_init__(self) -> None:
        if len(self.layers) <= 1:
            raise ValueError("checkpoint segments must contain at least two layers")


_SquashPlanEntry = LayerRef | CheckpointSegment


@dataclass(frozen=True)
class SquashPlan:
    active_version: int
    active_layers: tuple[LayerRef, ...]
    entries: tuple[_SquashPlanEntry, ...]

    def __post_init__(self) -> None:
        if not self.active_layers:
            raise ValueError("active_layers must not be empty")
        if not self.entries:
            raise ValueError("entries must not be empty")
        if not self.checkpoint_segments:
            raise ValueError("squash plans must include at least one checkpoint segment")

    @property
    def checkpoint_segments(self) -> tuple[CheckpointSegment, ...]:
        return tuple(entry for entry in self.entries if isinstance(entry, CheckpointSegment))


class LayerCheckpointSquasher:
    """Plans runs between barrier layers and projects each run into a checkpoint layer."""

    def __init__(
        self,
        storage_root: str | Path,
    ) -> None:
        self._storage_root = Path(storage_root)
        self._view = MergedView(self._storage_root)

    def plan(
        self,
        active_manifest: Manifest,
        *,
        max_depth: int,
        barrier_layers: tuple[LayerRef, ...] = (),
        min_reduction: int = 1,
    ) -> SquashPlan | None:
        if max_depth <= 0:
            raise ValueError("max_depth must be positive")
        if min_reduction <= 0:
            raise ValueError("min_reduction must be positive")
        if active_manifest.depth <= max_depth:
            return None

        entries = _segment_around_barriers(active_manifest.layers, barrier_layers)
        if len(entries) >= active_manifest.depth:
            return None
        if active_manifest.depth - len(entries) < min_reduction:
            return None
        checkpoint_segments = tuple(
            entry for entry in entries if isinstance(entry, CheckpointSegment)
        )
        if len(entries) > max_depth and all(
            len(segment.layers) <= max_depth for segment in checkpoint_segments
        ):
            return None

        return SquashPlan(
            active_version=active_manifest.version,
            active_layers=active_manifest.layers,
            entries=entries,
        )

    def build_checkpoint(
        self,
        segment: CheckpointSegment,
        *,
        active_version: int,
    ) -> LayerRef:
        layer_id, staging_dir, layer_dir = self._allocate_checkpoint_paths(active_version + 1)
        segment_manifest = Manifest(
            version=active_version,
            layers=segment.layers,
        )
        try:
            self._view.materialize(staging_dir, segment_manifest)
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
        fsync_path(layer_dir.parent)
        return LayerRef(layer_id=layer_id, path=f"{LAYERS_DIR}/{layer_id}")

    def discard_checkpoint(self, checkpoint: LayerRef) -> None:
        layer_path = resolve_storage_path(self._storage_root, checkpoint.path)
        shutil.rmtree(layer_path, ignore_errors=True)

    def _allocate_checkpoint_paths(self, next_version: int) -> tuple[str, Path, Path]:
        return allocate_unique_layer_paths(
            storage_root=self._storage_root,
            layers_dir=LAYERS_DIR,
            staging_dir=STAGING_DIR,
            next_version=next_version,
            id_factory=_default_checkpoint_id,
        )


def _segment_around_barriers(
    layers: tuple[LayerRef, ...],
    barrier_layers: tuple[LayerRef, ...],
) -> tuple[_SquashPlanEntry, ...]:
    barriers = set(barrier_layers)
    entries: list[_SquashPlanEntry] = []
    run: list[LayerRef] = []

    def flush_run() -> None:
        if len(run) > 1:
            entries.append(CheckpointSegment(tuple(run)))
        elif run:
            entries.append(run[0])
        run.clear()

    for layer in layers:
        if layer in barriers:
            flush_run()
            entries.append(layer)
        else:
            run.append(layer)
    flush_run()
    return tuple(entries)


def manifest_prefix_before_plan(
    manifest: Manifest,
    plan: SquashPlan,
) -> tuple[LayerRef, ...] | None:
    planned_depth = len(plan.active_layers)
    if planned_depth > manifest.depth:
        return None
    if manifest.layers[-planned_depth:] != plan.active_layers:
        return None
    return manifest.layers[:-planned_depth]


def _default_checkpoint_id(next_version: int) -> str:
    return f"B{next_version:06d}-{uuid.uuid4().hex[:8]}"


__all__ = [
    "CheckpointSegment",
    "LayerCheckpointSquasher",
    "SquashPlan",
    "manifest_prefix_before_plan",
]
