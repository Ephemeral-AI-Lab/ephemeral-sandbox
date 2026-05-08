"""Policy-blind immutable layer publisher."""

from __future__ import annotations

import hashlib
import os
import shutil
import time
import uuid
from collections.abc import Callable, Sequence
from pathlib import Path, PurePosixPath

from sandbox.layer_stack.filesystem import join_layer_path, remove_path
from sandbox.layer_stack.layer.change import LayerChange
from sandbox.layer_stack.layer.index import OPAQUE_MARKER, WHITEOUT_PREFIX
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
from sandbox.layer_stack.timing import record_elapsed


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

    def publish_layer_locked(
        self,
        changes: Sequence[LayerChange],
        *,
        expected_manifest: Manifest,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        total_start = time.perf_counter()
        read_active_start = time.perf_counter()
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

        digest_start = time.perf_counter()
        layer_digest = _changes_digest(changes)
        if _head_layer_digest(self._storage_root, active) == layer_digest:
            record_elapsed(timings, "layer_stack.publish.digest_check_s", digest_start)
            record_elapsed(timings, "layer_stack.publish.idempotent_s", total_start)
            record_elapsed(timings, "layer_stack.publish.total_s", total_start)
            return active
        record_elapsed(timings, "layer_stack.publish.digest_check_s", digest_start)

        allocate_start = time.perf_counter()
        layer_id, staging_dir, layer_dir = self._allocate_layer_paths(active.version + 1)
        record_elapsed(
            timings,
            "layer_stack.publish.allocate_layer_paths_s",
            allocate_start,
        )
        create_staging_start = time.perf_counter()
        staging_dir.mkdir(parents=True)
        record_elapsed(timings, "layer_stack.publish.create_staging_s", create_staging_start)
        try:
            write_changes_start = time.perf_counter()
            for change in changes:
                self._write_change(staging_dir, change)
            record_elapsed(
                timings,
                "layer_stack.publish.write_changes_s",
                write_changes_start,
            )
            replace_start = time.perf_counter()
            layer_dir.parent.mkdir(parents=True, exist_ok=True)
            os.replace(staging_dir, layer_dir)
            _write_layer_digest(self._storage_root, layer_id, layer_digest)
            record_elapsed(
                timings,
                "layer_stack.publish.replace_staging_s",
                replace_start,
            )
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
        read_latest_start = time.perf_counter()
        latest = read_manifest(self._manifest_file)
        record_elapsed(
            timings,
            "layer_stack.publish.read_latest_manifest_s",
            read_latest_start,
        )
        if latest != active:
            remove_path(layer_dir)
            _digest_path(self._storage_root, layer_id).unlink(missing_ok=True)
            raise ManifestConflictError(
                "active manifest changed during layer publish: "
                f"expected version {active.version}, found version {latest.version}"
            )
        write_manifest_start = time.perf_counter()
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
            marker = join_layer_path(layer_dir, change.path) / OPAQUE_MARKER
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
        target = join_layer_path(layer_dir, change.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        remove_path(target)
        target.write_bytes(content)

    def _write_symlink(self, layer_dir: Path, change: LayerChange) -> None:
        if change.source_path is None:
            raise ValueError("symlink changes require source_path target")
        target = join_layer_path(layer_dir, change.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(change.source_path, target)


def _default_layer_id(next_version: int) -> str:
    return f"L{next_version:06d}-{uuid.uuid4().hex[:8]}"


def _changes_digest(changes: Sequence[LayerChange]) -> str:
    digest = hashlib.sha256()
    for change in changes:
        digest.update(change.kind.encode("utf-8"))
        digest.update(b"\0")
        digest.update(change.path.encode("utf-8"))
        digest.update(b"\0")
        if change.kind == "write":
            if change.source_path is None:
                raise ValueError("write changes require source_path")
            digest.update(Path(change.source_path).read_bytes())
        elif change.kind == "symlink":
            digest.update(str(change.source_path or "").encode("utf-8"))
        digest.update(b"\0")
    return digest.hexdigest()


def _metadata_dir(storage_root: Path) -> Path:
    return storage_root / ".layer-metadata"


def _digest_path(storage_root: Path, layer_id: str) -> Path:
    return _metadata_dir(storage_root) / f"{layer_id}.digest"


def _write_layer_digest(storage_root: Path, layer_id: str, digest: str) -> None:
    metadata = _metadata_dir(storage_root)
    metadata.mkdir(parents=True, exist_ok=True)
    _digest_path(storage_root, layer_id).write_text(digest, encoding="utf-8")


def _head_layer_digest(storage_root: Path, active: Manifest) -> str | None:
    if not active.layers:
        return None
    try:
        return _digest_path(storage_root, active.layers[0].layer_id).read_text(
            encoding="utf-8",
        )
    except OSError:
        return None


def _whiteout_path(layer_dir: Path, rel: str) -> Path:
    target = PurePosixPath(rel)
    parent_parts = tuple(part for part in target.parent.parts if part != ".")
    whiteout = layer_dir.joinpath(*parent_parts, f"{WHITEOUT_PREFIX}{target.name}")
    whiteout.parent.mkdir(parents=True, exist_ok=True)
    return whiteout
