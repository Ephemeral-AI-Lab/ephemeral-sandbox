"""Policy-blind immutable layer publisher."""

from __future__ import annotations

import hashlib
import os
import shutil
import uuid
from collections.abc import Callable, Sequence
from pathlib import Path, PurePosixPath

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.lease_budget import BudgetDecision
from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    ManifestConflictError,
    read_manifest,
    write_manifest_atomic,
)
from sandbox.layer_stack.merged_view import OPAQUE_MARKER, WHITEOUT_PREFIX


class CommitBackpressureError(RuntimeError):
    """Raised when lease pressure blocks publishing a new layer."""

    def __init__(self, decision: BudgetDecision) -> None:
        super().__init__(decision.reason)
        self.decision = decision


class LayerPublisher:
    """Writes accepted changes into immutable layers and publishes manifests."""

    def __init__(
        self,
        storage_root: str | Path,
        manifest_file: str | Path,
        *,
        id_factory: Callable[[int], str] | None = None,
        backpressure_checker: Callable[[Manifest], BudgetDecision] | None = None,
    ) -> None:
        self._storage_root = Path(storage_root)
        self._manifest_file = Path(manifest_file)
        self._id_factory = id_factory or _default_layer_id
        self._backpressure_checker = backpressure_checker

    def publish_layer_locked(
        self,
        changes: Sequence[LayerChange],
        *,
        expected_manifest: Manifest,
    ) -> Manifest:
        active = read_manifest(self._manifest_file)
        if active != expected_manifest:
            raise ManifestConflictError(
                "active manifest changed before layer publish: "
                f"expected version {expected_manifest.version}, "
                f"found version {active.version}"
            )
        if not changes:
            return active

        self._check_backpressure(active)
        layer_id, staging_dir, layer_dir = self._allocate_layer_paths(active.version + 1)
        staging_dir.mkdir(parents=True)
        try:
            for change in changes:
                self._write_change(staging_dir, change)
            layer_dir.parent.mkdir(parents=True, exist_ok=True)
            os.replace(staging_dir, layer_dir)
        except Exception:
            shutil.rmtree(staging_dir, ignore_errors=True)
            raise

        new_manifest = Manifest(
            version=active.version + 1,
            layers=(
                LayerRef(layer_id=layer_id, path=f"{LAYERS_DIR}/{layer_id}"),
                *active.layers,
            ),
        )
        latest = read_manifest(self._manifest_file)
        if latest != active:
            raise ManifestConflictError(
                "active manifest changed during layer publish: "
                f"expected version {active.version}, found version {latest.version}"
            )
        write_manifest_atomic(self._manifest_file, new_manifest)
        return new_manifest

    def _check_backpressure(self, active: Manifest) -> None:
        if self._backpressure_checker is None:
            return
        decision = self._backpressure_checker(active)
        if decision.kind == "backpressure_commits":
            raise CommitBackpressureError(decision)

    def _allocate_layer_paths(self, next_version: int) -> tuple[str, Path, Path]:
        for _ in range(100):
            layer_id = self._id_factory(next_version)
            layer_dir = self._storage_root / LAYERS_DIR / layer_id
            staging_dir = self._storage_root / STAGING_DIR / f"{layer_id}.staging"
            if not layer_dir.exists() and not staging_dir.exists():
                return layer_id, staging_dir, layer_dir
        raise RuntimeError("could not allocate a unique layer id")

    def _write_change(self, layer_dir: Path, change: LayerChange) -> None:
        if change.kind == "write":
            self._write_file(layer_dir, change)
        elif change.kind == "delete":
            _whiteout_path(layer_dir, change.path).write_text("", encoding="utf-8")
        elif change.kind == "symlink":
            self._write_symlink(layer_dir, change)
        elif change.kind == "opaque_dir":
            marker = _join_rel(layer_dir, change.path) / OPAQUE_MARKER
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text("", encoding="utf-8")
        else:
            raise ValueError(f"unsupported layer change kind: {change.kind}")

    def _write_file(self, layer_dir: Path, change: LayerChange) -> None:
        if change.source_path is None:
            raise ValueError("write changes require source_path")
        source = Path(change.source_path)
        content = source.read_bytes()
        if change.content_hash and hashlib.sha256(content).hexdigest() != change.content_hash:
            raise ValueError(f"content hash mismatch for {change.path}")
        target = _join_rel(layer_dir, change.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)

    def _write_symlink(self, layer_dir: Path, change: LayerChange) -> None:
        if change.source_path is None:
            raise ValueError("symlink changes require source_path target")
        target = _join_rel(layer_dir, change.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(change.source_path, target)


def _default_layer_id(next_version: int) -> str:
    return f"L{next_version:06d}-{uuid.uuid4().hex[:8]}"


def _join_rel(root: Path, rel: str) -> Path:
    return root.joinpath(*PurePosixPath(rel).parts)


def _whiteout_path(layer_dir: Path, rel: str) -> Path:
    target = PurePosixPath(rel)
    parent_parts = tuple(part for part in target.parent.parts if part != ".")
    whiteout = layer_dir.joinpath(*parent_parts, f"{WHITEOUT_PREFIX}{target.name}")
    whiteout.parent.mkdir(parents=True, exist_ok=True)
    return whiteout
