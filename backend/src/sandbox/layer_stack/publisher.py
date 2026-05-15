"""Policy-blind immutable layer publisher."""

from __future__ import annotations

import hashlib
import os
import shutil
import uuid
from collections.abc import Callable, Sequence
from pathlib import Path

from sandbox.layer_stack.paths import allocate_unique_layer_paths, fsync_path, remove_path
from sandbox.layer_stack.changes import (
    LayerChange,
    PreparedLayerChange,
    aggregate_layer_changes,
    prepare_layer_change,
    update_digest,
    write_layer_change,
)
from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    ManifestConflictError,
    manifest_path,
    read_manifest,
    write_manifest_atomic,
)
from sandbox._shared.clock import monotonic_now, record_elapsed


class LayerPublisher:
    """Writes accepted changes into immutable layers and publishes manifests."""

    def __init__(
        self,
        storage_root: str | Path,
        *,
        id_factory: Callable[[int], str] | None = None,
    ) -> None:
        self._storage_root = Path(storage_root)
        self._manifest_file = manifest_path(self._storage_root)
        self._id_factory = id_factory or _default_layer_id

    def publish_layer(
        self,
        changes: Sequence[LayerChange],
        *,
        expected_manifest: Manifest,
        source_root: str | Path | None = None,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        total_start = monotonic_now()
        read_active_start = monotonic_now()
        active = read_manifest(self._manifest_file)
        record_elapsed(timings, "layer_stack.publish.read_active_manifest_s", read_active_start)
        if active != expected_manifest:
            raise ManifestConflictError(
                "active manifest changed before layer publish: "
                f"expected version {expected_manifest.version}, "
                f"found version {active.version}"
            )
        if not changes:
            record_elapsed(timings, "layer_stack.publish.total_s", total_start)
            return active

        prepare_start = monotonic_now()
        prepared_changes, layer_digest = _prepare_changes(
            changes,
            source_root=source_root,
        )
        if _head_layer_digest(self._storage_root, active) == layer_digest:
            _record_prepare_elapsed(timings, prepare_start)
            record_elapsed(timings, "layer_stack.publish.idempotent_s", total_start)
            record_elapsed(timings, "layer_stack.publish.total_s", total_start)
            return active
        _record_prepare_elapsed(timings, prepare_start)

        allocate_start = monotonic_now()
        layer_id, staging_dir, layer_dir = self._allocate_layer_paths(active.version + 1)
        record_elapsed(timings, "layer_stack.publish.allocate_layer_paths_s", allocate_start)
        create_staging_start = monotonic_now()
        staging_dir.mkdir(parents=True)
        record_elapsed(timings, "layer_stack.publish.create_staging_s", create_staging_start)
        try:
            write_changes_start = monotonic_now()
            for prepared in prepared_changes:
                write_layer_change(prepared, staging_dir)
            _fsync_tree_files(staging_dir)
            fsync_path(staging_dir)
            record_elapsed(timings, "layer_stack.publish.write_changes_s", write_changes_start)
            replace_start = monotonic_now()
            layer_dir.parent.mkdir(parents=True, exist_ok=True)
            os.replace(staging_dir, layer_dir)
            fsync_path(layer_dir.parent)
            _write_layer_digest(self._storage_root, layer_id, layer_digest)
            record_elapsed(timings, "layer_stack.publish.replace_staging_s", replace_start)
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
        read_latest_start = monotonic_now()
        latest = read_manifest(self._manifest_file)
        record_elapsed(timings, "layer_stack.publish.read_latest_manifest_s", read_latest_start)
        if latest != active:
            remove_path(layer_dir)
            _digest_path(self._storage_root, layer_id).unlink(missing_ok=True)
            raise ManifestConflictError(
                "active manifest changed during layer publish: "
                f"expected version {active.version}, found version {latest.version}"
            )
        write_manifest_start = monotonic_now()
        try:
            write_manifest_atomic(self._manifest_file, new_manifest)
        except Exception:
            remove_path(layer_dir)
            _digest_path(self._storage_root, layer_id).unlink(missing_ok=True)
            raise
        record_elapsed(timings, "layer_stack.publish.write_manifest_s", write_manifest_start)
        record_elapsed(timings, "layer_stack.publish.total_s", total_start)
        return new_manifest

    def _allocate_layer_paths(self, next_version: int) -> tuple[str, Path, Path]:
        return allocate_unique_layer_paths(
            storage_root=self._storage_root,
            layers_dir=LAYERS_DIR,
            staging_dir=STAGING_DIR,
            next_version=next_version,
            id_factory=self._id_factory,
        )


def _default_layer_id(next_version: int) -> str:
    return f"L{next_version:06d}-{uuid.uuid4().hex[:8]}"


def _record_prepare_elapsed(
    timings: dict[str, float] | None,
    prepare_start: float,
) -> None:
    if timings is None:
        return
    timings["layer_stack.publish.prepare_changes_s"] = monotonic_now() - prepare_start


def _prepare_changes(
    changes: Sequence[LayerChange],
    *,
    source_root: str | Path | None = None,
) -> tuple[tuple[PreparedLayerChange, ...], str]:
    digest = hashlib.sha256()
    resolved_source_root = (
        Path(source_root).resolve(strict=True) if source_root is not None else None
    )
    prepared: list[PreparedLayerChange] = []
    for change in aggregate_layer_changes(changes):
        prepared_change = prepare_layer_change(change, source_root=resolved_source_root)
        update_digest(digest, prepared_change)
        prepared.append(prepared_change)
    return tuple(prepared), digest.hexdigest()


def _metadata_dir(storage_root: Path) -> Path:
    return storage_root / ".layer-metadata"


def _digest_path(storage_root: Path, layer_id: str) -> Path:
    return _metadata_dir(storage_root) / f"{layer_id}.digest"


def _write_layer_digest(storage_root: Path, layer_id: str, digest: str) -> None:
    metadata = _metadata_dir(storage_root)
    metadata.mkdir(parents=True, exist_ok=True)
    target = _digest_path(storage_root, layer_id)
    data = digest.encode("utf-8")
    fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    try:
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    fsync_path(metadata)


def _fsync_tree_files(root: Path) -> None:
    """fsync every regular file under *root* (skip symlinks)."""
    for current_root, _dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(current_root)
        for filename in filenames:
            file_path = current / filename
            if not file_path.is_symlink():
                fsync_path(file_path)


def _head_layer_digest(storage_root: Path, active: Manifest) -> str | None:
    if not active.layers:
        return None
    try:
        return _digest_path(storage_root, active.layers[0].layer_id).read_text(
            encoding="utf-8",
        )
    except OSError:
        return None


__all__ = ["LayerPublisher"]
